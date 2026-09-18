//! 录屏：把**本软件窗口那一块**画面 + 麦克风（可选摄像头画中画）录成 mp4。
//!
//! # 为什么是 ffmpeg 子进程
//!
//! 2026-09-18 在上面这台机器上实测了五条路，只有一条成立：
//!
//! | 路线 | 结果 |
//! | --- | --- |
//! | `gdigrab -i title=窗口名` | ❌ 全黑（YAVG=12）：窗口 DC 抓不到 GPUI 的 D3D/DComp 画面 |
//! | `ddagrab`（桌面复制，GPU 路径） | ❌ 本机装了 ToDesk 虚拟显示器，DXGI 0/1/2 号适配器**都没有输出**，全报 `Selected output not supported` |
//! | `Window::render_to_image()` | ❌ 是 `test-support` 特性下的桩，Windows 平台没实现，直接 bail |
//! | WGC（Windows.Graphics.Capture） | ⚠️ 可行且最干净，但要手写 D3D11 + 帧池 + 编码器，与「要简单」冲突 |
//! | **`gdigrab -offset/-video_size`** | ✅ 与「全屏图同坐标裁剪」**逐像素一致**（三个区域 PSNR = inf），且原生分辨率 |
//!
//! 于是坐标用**物理像素**：`GetWindowRect` 在外壳这个 DPI-aware 进程里拿到的就是物理像素，
//! 与采集空间一致。（踩过的坑：gdigrab 自己的 `-offset` 在 ffmpeg 这个 **DPI-unaware**
//! 进程里是「逻辑像素」，150% 缩放下差 1.5 倍 —— 所以绝不能靠 gpui 的逻辑坐标去算。）
//!
//! # 暂停/继续为什么是「分段 + 拼接」
//!
//! 单个 ffmpeg 进程没有原生暂停：`sendcmd` 管不到实时源，挂起进程会把实时时钟搞乱。
//! 所以暂停 = 优雅收掉这一段（**stdin 写 `q`，不用 kill** —— 强杀会留下没有 moov
//! 的坏文件；实测 `q` 退出码 0、收尾 0.16~0.88s、文件完整），继续 = 开新的一段。
//! 停止时用 concat 解复用器 `-c copy` **无损拼接**（实测 3.0s + 2.43s → 5.456s，
//! 93 帧视频 / 46 帧音频，全片解码零错误）。
//!
//! # 画中画为什么是「录完再合成」
//!
//! 实测把 dshow 摄像头直接接进 overlay 滤镜（实时合成）会**把时间轴搞乱**：
//! 195 帧只占 3.56s（放出来是快进），因为摄像头的 PTS 与墙钟对不上。
//! 所以录制期摄像头**单独写一路文件**，停止后做**文件到文件**合成（时间轴确定）：
//! 实测 7 秒素材用 `h264_nvenc` 合成只要 0.44s。
//!
//! 摄像头缺失/被占用 → 自动退化成只录画面，并在状态栏说一句，不静默失败。

use std::io::Write as _;
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// 成品落在哪（按用户要求）。
pub const OUT_DIR: &str = r"D:\录屏";
/// `D:\录屏` 建不出来时的退路（普通用户目录下的 Videos）。
pub const FALLBACK_OUT_DIR: &str = "typst-live";
/// 采集帧率。实测只采集不编码的极限是 23~30fps（卡在 gdigrab 的 BitBlt，
/// 换 nvenc/superfast 都没用），30 是贴着上限的数。
pub const FPS: u32 = 30;
/// 采不到这么小就没必要录（也躲开 yuv420p 的偶数与滤镜的下限）。
pub const MIN_SIDE: i32 = 64;
/// 画中画的宽与边距（像素，相对采集区）。
pub const PIP_WIDTH: u32 = 320;
pub const PIP_MARGIN: u32 = 16;
/// 段文件目录前缀（藏在成品目录里，收尾后整个删掉）。
const TEMP_PREFIX: &str = ".tmp-";
/// `CREATE_NO_WINDOW`：外壳是 GUI 子系统，不设这个每起一个 ffmpeg 都会闪一个黑框。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 等 ffmpeg 优雅收尾的上限。实测 0.16~0.88s，给足余量；真卡住就强杀。
const FINISH_TIMEOUT: Duration = Duration::from_secs(8);

// ---------------------------------------------------------------------------
// 采集区域
// ---------------------------------------------------------------------------

/// 一块矩形（物理像素，屏幕坐标系）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    /// 右边界 / 下边界（左闭右开）。
    fn right(&self) -> i32 {
        self.x + self.w
    }
    fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

/// 窗口矩形 ∩ 显示器矩形，取偶、查最小尺寸。
///
/// 为什么要交：窗口可以比屏幕大、也可以被拖出去一半（实测过窗口底边 1608 而屏幕只有 1600），
/// 直接拿去采集要么报错要么出黑边。
///
/// 为什么要取偶：`yuv420p` 的宽高必须是偶数，否则 libx264 直接拒绝。
pub fn crop_region(win: Rect, monitor: Rect, min_side: i32) -> Option<Rect> {
    let x = win.x.max(monitor.x);
    let y = win.y.max(monitor.y);
    let right = win.right().min(monitor.right());
    let bottom = win.bottom().min(monitor.bottom());

    // 取偶（向下取整）：奇数宽度会编不出 yuv420p
    let w = (right - x) & !1;
    let h = (bottom - y) & !1;
    if w < min_side || h < min_side {
        return None;
    }
    Some(Rect { x, y, w, h })
}

// ---------------------------------------------------------------------------
// 设备枚举
// ---------------------------------------------------------------------------

/// dshow 里挑出来的设备名。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Devices {
    pub video: Vec<String>,
    pub audio: Vec<String>,
}

/// 解析 `ffmpeg -list_devices true -f dshow -i dummy` 的 stderr（它把清单打在 stderr 上）。
///
/// 真实输出的样子（注意前缀与 Alternative name 行）：
/// ```text
/// [in#0 @ 000001a3c36223c0] "Integrated Webcam" (video)
/// [in#0 @ 000001a3c36223c0]   Alternative name "@device_pnp_\\?\usb#vid_0c45&..."
/// [in#0 @ 000001a3c36223c0] "麦克风 (Realtek(R) Audio)" (audio)
/// ```
pub fn parse_dshow_devices(stderr: &str) -> Devices {
    let mut out = Devices::default();
    for line in stderr.lines() {
        let kind = if line.trim_end().ends_with("(video)") {
            &mut out.video
        } else if line.trim_end().ends_with("(audio)") {
            &mut out.audio
        } else {
            continue;
        };
        // 名字是 `] ` 之后那对引号里的东西；Alternative name 行不以设备名开头，会被上面的
        // ends_with 过滤掉，但这里的判断仍然只看「第一个引号对」。
        let Some(rest) = line.rsplit("] ").next() else {
            continue;
        };
        let Some(start) = rest.find('"') else {
            continue;
        };
        let Some(end) = rest[start + 1..].find('"') else {
            continue;
        };
        kind.push(rest[start + 1..start + 1 + end].to_string());
    }
    out
}

/// 枚举一次设备。失败（ffmpeg 不在、dshow 抽风）返回默认空表。
pub fn list_devices() -> Devices {
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-list_devices",
        "true",
        "-f",
        "dshow",
        "-i",
        "dummy",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    hide_console(&mut cmd);
    match cmd.output() {
        Ok(out) => parse_dshow_devices(&String::from_utf8_lossy(&out.stderr)),
        Err(_) => Devices::default(),
    }
}

/// ffmpeg 在不在（缓存一次：这是每次 render 都可能问的问题）。
pub fn ffmpeg_available() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_console(&mut cmd);
        cmd.status().is_ok_and(|s| s.success())
    })
}

/// 有没有能用的硬件编码器（合成那一步用得上）。
///
/// 只用来决定「合成」这一步用谁：录制的画面**始终**用 libx264（x264 的文字边缘更稳，
/// 而且实测瓶颈在 gdigrab 的 BitBlt，换硬件编码一点不快）。
pub fn nvenc_available() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=black:s=64x64:d=0.1",
            "-frames:v",
            "1",
            "-c:v",
            "h264_nvenc",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        hide_console(&mut cmd);
        cmd.status().is_ok_and(|s| s.success())
    })
}

// ---------------------------------------------------------------------------
// 命令行构造（纯函数，好单测）
// ---------------------------------------------------------------------------

/// 画面（+ 麦克风）那一路。
pub fn screen_args(region: Rect, mic: Option<&str>, fps: u32, out: &Path) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-f".into(),
        "gdigrab".into(),
        "-framerate".into(),
        fps.to_string(),
        "-offset_x".into(),
        region.x.to_string(),
        "-offset_y".into(),
        region.y.to_string(),
        "-video_size".into(),
        format!("{}x{}", region.w, region.h),
        "-i".into(),
        "desktop".into(),
    ];
    if let Some(mic) = mic {
        a.extend([
            "-f".into(),
            "dshow".into(),
            "-i".into(),
            format!("audio={mic}"),
        ]);
    }
    a.extend(["-map".into(), "0:v".into()]);
    if mic.is_some() {
        a.extend(["-map".into(), "1:a".into()]);
    }
    a.extend([
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        "20".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
    ]);
    if mic.is_some() {
        // `aresample=async=1:first_pts=0`：DShow 的采样时钟与 gdigrab 的墙钟不是一个，
        // 不对齐的话首帧音频会带一个巨大的负 PTS（播放器表现为开头一段静音/音画错位）。
        a.extend([
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "128k".into(),
            "-af".into(),
            "aresample=async=1:first_pts=0".into(),
        ]);
    }
    a.extend(["-movflags".into(), "+faststart".into(), path_str(out)]);
    a
}

/// 摄像头那一路（单独一个进程、单独一个文件，收尾时才合成进去）。
pub fn camera_args(device: &str, out: &Path) -> Vec<String> {
    vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-f".into(),
        "dshow".into(),
        "-video_size".into(),
        "640x480".into(),
        "-framerate".into(),
        FPS.to_string(),
        "-i".into(),
        format!("video={device}"),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        "23".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-movflags".into(),
        "+faststart".into(),
        path_str(out),
    ]
}

/// concat 解复用器的清单文本。
pub fn concat_list_text(files: &[PathBuf]) -> String {
    // 单引号里再出现单引号要转义成 `'\''`（Windows 路径一般不会，但不做就是隐患）。
    files
        .iter()
        .map(|p| format!("file '{}'\n", path_str(p).replace('\'', r"'\''")))
        .collect()
}

/// 拼接（无损 remux，不解码）。
pub fn concat_args(list: &Path, out: &Path) -> Vec<String> {
    vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-f".into(),
        "concat".into(),
        "-safe".into(),
        "0".into(),
        "-i".into(),
        path_str(list),
        "-c".into(),
        "copy".into(),
        "-movflags".into(),
        "+faststart".into(),
        path_str(out),
    ]
}

/// 画中画合成（文件到文件）：摄像头缩到右下角，视频重编码，音频直接 copy。
pub fn overlay_args(screen: &Path, cam: &Path, out: &Path, nvenc: bool) -> Vec<String> {
    let filter = format!(
        "[1:v]scale={PIP_WIDTH}:-2,setsar=1[pip];\
         [0:v][pip]overlay=W-w-{PIP_MARGIN}:H-h-{PIP_MARGIN}:eof_action=pass,format=yuv420p[v]"
    );
    let encoder: Vec<String> = if nvenc {
        vec![
            "-c:v".into(),
            "h264_nvenc".into(),
            "-preset".into(),
            "p4".into(),
            "-cq".into(),
            "23".into(),
        ]
    } else {
        vec![
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-crf".into(),
            "20".into(),
        ]
    };
    let mut a: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-i".into(),
        path_str(screen),
        "-i".into(),
        path_str(cam),
        "-filter_complex".into(),
        filter,
        "-map".into(),
        "[v]".into(),
        "-map".into(),
        "0:a?".into(), // 没声音也不能让整条命令失败
    ];
    a.extend(encoder);
    a.extend([
        "-c:a".into(),
        "copy".into(),
        "-movflags".into(),
        "+faststart".into(),
        path_str(out),
    ]);
    a
}

/// 时长标签 `01:23` / `1:02:03`。
pub fn elapsed_label(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

// ---------------------------------------------------------------------------
// 状态机与分段
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Recording,
    Paused,
    /// 已经收掉 ffmpeg，正在做拼接/合成（这一步可能几秒，所以有独立状态）。
    Finalizing,
}

/// 一段录像。
///
/// 生命周期：`start` → (`pause` → `resume`)… → `stop` → [`Finalize::run`]。
/// 录制期只碰文件与子进程，不碰 UI；收尾那一步跑在后台线程里。
pub struct Recorder {
    /// 段文件目录（藏在成品目录里的 `.tmp-*`）。
    dir: PathBuf,
    /// 成品路径。
    out: PathBuf,
    region: Rect,
    mic: Option<String>,
    cam: Option<String>,
    state: State,
    /// 已经写完的段数（每段两个文件：`s{n}.mp4` / `c{n}.mp4`）。
    sealed: u32,
    /// 正在跑的 ffmpeg（画面一个、摄像头一个）。
    live: Vec<Child>,
    /// 累计录制时长（不含暂停）。
    elapsed: Duration,
    last_tick: Option<Instant>,
    /// 给状态栏的一句话（出错、退化、成品路径都走这里）。
    note: Option<String>,
}

impl Recorder {
    /// 开录第一段。`mic` / `cam` 是设备名，`None` = 不录这一路。
    pub fn start(
        out_root: &Path,
        region: Rect,
        mic: Option<String>,
        cam: Option<String>,
        fps: u32,
    ) -> Result<Self, String> {
        if !ffmpeg_available() {
            return Err("PATH 上找不到 ffmpeg —— 录屏要用它，先装一个再点".into());
        }
        if region.w < MIN_SIDE || region.h < MIN_SIDE {
            return Err(format!(
                "窗口太小（{}x{}），至少 {}x{} 才录",
                region.w, region.h, MIN_SIDE, MIN_SIDE
            ));
        }

        let stem = format!("typst-live-{}", local_stamp());
        let dir = out_root.join(format!("{TEMP_PREFIX}{stem}"));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("建不了临时目录「{}」：{e}", dir.display()))?;

        let mut rec = Self {
            dir,
            out: out_root.join(format!("{stem}.mp4")),
            region,
            mic,
            cam,
            state: State::Recording,
            sealed: 0,
            live: Vec::new(),
            elapsed: Duration::ZERO,
            last_tick: Some(Instant::now()),
            note: None,
        };
        rec.spawn_segment(fps)?;
        rec.note = Some("录制中".into());
        Ok(rec)
    }

    fn spawn_segment(&mut self, fps: u32) -> Result<(), String> {
        let n = self.sealed + 1;
        let screen = self.dir.join(format!("s{n}.mp4"));

        let mut cmd = Command::new("ffmpeg");
        cmd.args(screen_args(self.region, self.mic.as_deref(), fps, &screen))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(ffmpeg_log(&self.dir, n, "screen"));
        hide_console(&mut cmd);
        let child = cmd.spawn().map_err(|e| format!("起不了 ffmpeg：{e}"))?;
        self.live.push(child);

        // 摄像头那一路失败**不算错**：退化掉，继续录画面（用户体验上比整场录不了好得多）。
        if let Some(dev) = self.cam.clone() {
            let cam_out = self.dir.join(format!("c{n}.mp4"));
            let mut cmd = Command::new("ffmpeg");
            cmd.args(camera_args(&dev, &cam_out))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(ffmpeg_log(&self.dir, n, "camera"));
            hide_console(&mut cmd);
            match cmd.spawn() {
                Ok(child) => self.live.push(child),
                Err(e) => {
                    self.cam = None;
                    self.note = Some(format!("摄像头起不来（{e}），这次只录画面"));
                }
            }
        }
        Ok(())
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// 每秒调一次：累加时长、看看 ffmpeg 是不是偷偷死了。
    ///
    /// 返回值是「这一拍新发生的事」——调用方（状态栏）该说一句的那种，
    /// 没有就 `None`（不然每秒覆写一次状态栏，别的提示就永远看不见了）。
    pub fn tick(&mut self) -> Option<String> {
        if let Some(last) = self.last_tick.take()
            && self.state == State::Recording
        {
            self.elapsed += last.elapsed();
        }
        self.last_tick = Some(Instant::now());

        if self.state != State::Recording {
            return None;
        }
        // 画面那一路死了 = 这次录不成了（摄像头那一路死了只退化画中画）。
        let mut dead: Vec<usize> = Vec::new();
        for (i, child) in self.live.iter_mut().enumerate() {
            if matches!(child.try_wait(), Ok(Some(_))) {
                dead.push(i);
            }
        }
        if dead.is_empty() {
            return None;
        }
        let screen_died = dead.contains(&0);
        for i in dead.into_iter().rev() {
            // 拿走的同时 `wait()` 一下：它是刚退出的子进程，不 wait 会留一个僵尸。
            let mut child = self.live.remove(i);
            let _ = child.wait();
        }
        if screen_died {
            self.state = State::Paused;
            self.note = Some("录屏中断了（ffmpeg 自己退了）——已写入的部分还在，点停止收尾".into());
        } else {
            self.cam = None;
            self.note = Some("摄像头断了，画中画这次不做，画面继续录".into());
        }
        self.note.clone()
    }

    /// 暂停：优雅收掉当前段（`q`），落成一份完整文件。
    pub fn pause(&mut self) -> Result<(), String> {
        if self.state != State::Recording {
            return Ok(());
        }
        self.finish_live();
        self.sealed += 1;
        self.state = State::Paused;
        self.note = Some("已暂停".into());
        Ok(())
    }

    /// 继续：开新的一段（拼接时按段顺序接上，所以暂停处不会有空洞）。
    pub fn resume(&mut self, fps: u32) -> Result<(), String> {
        if self.state != State::Paused {
            return Ok(());
        }
        self.live.clear();
        self.spawn_segment(fps)?;
        self.state = State::Recording;
        self.note = Some("录制中".into());
        Ok(())
    }

    /// 停止：收掉 ffmpeg，交出一份「收尾计划」让调用方丢到后台线程去跑。
    pub fn stop(&mut self) -> Result<Finalize, String> {
        if self.state == State::Finalizing {
            return Err("收尾还没跑完".into());
        }
        if self.state == State::Recording {
            self.finish_live();
            self.sealed += 1;
        }
        self.state = State::Finalizing;

        if self.sealed == 0 {
            self.state = State::Idle;
            return Err("这一段什么都没录到".into());
        }
        if let Some(last) = self.last_tick.take() {
            self.elapsed += last.elapsed();
        }

        let screen: Vec<PathBuf> = (1..=self.sealed)
            .map(|n| self.dir.join(format!("s{n}.mp4")))
            .filter(|p| usable(p))
            .collect();
        if screen.is_empty() {
            self.state = State::Idle;
            return Err("录像文件是空的（ffmpeg 可能没跑起来）".into());
        }
        let cam: Vec<PathBuf> = if self.cam.is_some() {
            (1..=self.sealed)
                .map(|n| self.dir.join(format!("c{n}.mp4")))
                .filter(|p| usable(p))
                .collect()
        } else {
            Vec::new()
        };
        // 段数对不上（某段摄像头没写出来）→ 干脆不做画中画，别把时间轴拼歪。
        let cam = if cam.len() == screen.len() {
            cam
        } else {
            Vec::new()
        };

        Ok(Finalize {
            dir: self.dir.clone(),
            out: self.out.clone(),
            screen,
            cam,
            nvenc: nvenc_available(),
        })
    }

    /// 收掉所有还活着的 ffmpeg（写 `q` → 等 → 超时才杀）。
    fn finish_live(&mut self) {
        for child in self.live.iter_mut() {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(b"q");
                let _ = stdin.flush();
            }
            // 关掉 stdin：ffmpeg 只有在 stdin 关掉或收到 q 时才会写 moov 收尾
            child.stdin.take();
            finish_child(child);
        }
        self.live.clear();
    }

    /// 段目录（出错时告诉用户去哪找残留）。
    pub fn work_dir(&self) -> &Path {
        &self.dir
    }
}

/// 收尾计划：拼接 → 画中画合成 → 挪成成品。
///
/// 这是个**纯数据 + 阻塞执行**的东西，交给后台线程跑（几秒到几十秒），
/// 所以它不持有 `Recorder`，也不需要 UI。
pub struct Finalize {
    dir: PathBuf,
    out: PathBuf,
    screen: Vec<PathBuf>,
    cam: Vec<PathBuf>,
    nvenc: bool,
}

/// 收尾结果：成品路径 + 过程中要说的话（退化、部分失败）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub path: PathBuf,
    pub notes: Vec<String>,
}

impl Finalize {
    pub fn run(self) -> Result<Done, String> {
        let mut notes: Vec<String> = Vec::new();

        // ① 分段先各自接起来（无损 remux）。
        let screen = self.step_concat(&self.screen, "s", &mut notes)?;
        let cam = if self.cam.is_empty() {
            None
        } else {
            match self.step_concat(&self.cam, "c", &mut notes) {
                Ok(p) => Some(p),
                Err(e) => {
                    notes.push(format!("摄像头那一路拼接失败（{e}），这次不出画中画"));
                    None
                }
            }
        };

        // ② 有摄像头就合成；失败退回画面本体，别让整场录像白录。
        let mut final_video = screen.clone();
        if let Some(cam) = cam {
            let pip_out = self.dir.join("pip.mp4");
            match run_ffmpeg(&overlay_args(&screen, &cam, &pip_out, self.nvenc)) {
                Ok(()) if usable(&pip_out) => final_video = pip_out,
                Ok(()) => notes.push("画中画合成了空文件，这次不出画中画".into()),
                Err(e) => notes.push(format!("画中画合成失败（{e}），这次不出画中画")),
            }
        }

        // ③ 挪成成品（临时目录就在成品目录里，所以这一步是同一卷上的 rename，瞬间完成）。
        if let Some(parent) = self.out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::rename(&final_video, &self.out) {
            std::fs::copy(&final_video, &self.out)
                .map_err(|e2| format!("成品挪不过去：{e} / 复制也失败：{e2}"))?;
            let _ = std::fs::remove_file(&final_video);
        }

        // ④ 清场（段文件与日志）。清不掉也不影响成品，留个话。
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            notes.push(format!("临时目录没清掉（{}）：{e}", self.dir.display()));
        }
        Ok(Done {
            path: self.out,
            notes,
        })
    }

    fn step_concat(
        &self,
        files: &[PathBuf],
        tag: &str,
        notes: &mut Vec<String>,
    ) -> Result<PathBuf, String> {
        if files.len() == 1 {
            return Ok(files[0].clone());
        }
        let list = self.dir.join(format!("list-{tag}.txt"));
        std::fs::write(&list, concat_list_text(files))
            .map_err(|e| format!("写不了拼接清单：{e}"))?;
        let out = self.dir.join(format!("joined-{tag}.mp4"));
        run_ffmpeg(&concat_args(&list, &out))?;
        if !usable(&out) {
            return Err("拼接结果是空的".into());
        }
        if files.len() > 2 {
            notes.push(format!("{} 段合成一段", files.len()));
        }
        Ok(out)
    }
}

/// 文件存在且不是个空壳（ffmpeg 起不来时会留一个 0~几百字节的头）。
fn usable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() > 4096)
        .unwrap_or(false)
}

/// 起一个 ffmpeg 并等它跑完（收尾这几步都是文件到文件，不需要喂 stdin）。
fn run_ffmpeg(args: &[String]) -> Result<(), String> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    hide_console(&mut cmd);
    let out = cmd.output().map_err(|e| format!("起不了 ffmpeg：{e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let tail: Vec<&str> = err
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(2)
        .collect();
    Err(format!(
        "ffmpeg 退出码 {:?}：{}",
        out.status.code(),
        tail.join(" / ")
    ))
}

/// 给 ffmpeg 的 stderr 落一个日志文件（排查用；不占着管道，就不用担心写满阻塞子进程）。
fn ffmpeg_log(dir: &Path, n: u32, tag: &str) -> Stdio {
    match std::fs::File::create(dir.join(format!("ffmpeg-{tag}-{n}.log"))) {
        Ok(f) => Stdio::from(f),
        Err(_) => Stdio::null(),
    }
}

/// 等子进程自己退出；超时才强杀。返回它是否优雅退出。
fn finish_child(child: &mut Child) -> bool {
    let deadline = Instant::now() + FINISH_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn hide_console(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// 本地时间戳 `20260919-001234`（用 Win32 拿本地时间，免得为了一行格式化拉 chrono）。
fn local_stamp() -> String {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    // SAFETY: GetLocalTime 只写这个栈上结构，指针有效。
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

/// 成品目录：`D:\录屏`，建不出来就退到 `%USERPROFILE%\Videos\typst-live`。
pub fn out_dir() -> (PathBuf, Option<String>) {
    let preferred = PathBuf::from(OUT_DIR);
    if std::fs::create_dir_all(&preferred).is_ok() {
        return (preferred, None);
    }
    let fallback = std::env::var_os("USERPROFILE")
        .map(|p| PathBuf::from(p).join("Videos").join(FALLBACK_OUT_DIR))
        .unwrap_or_else(|| std::env::temp_dir().join(FALLBACK_OUT_DIR));
    let _ = std::fs::create_dir_all(&fallback);
    let note = format!("{OUT_DIR} 建不出来，这次落到「{}」", fallback.display());
    (fallback, Some(note))
}

// ---------------------------------------------------------------------------
// 窗口矩形（Win32）
// ---------------------------------------------------------------------------

/// 从 gpui 的窗口拿 HWND。
///
/// ⚠️ `Window` 自己有个同名方法 `window_handle()` 返回 gpui 的 `AnyWindowHandle`，
/// **会把 trait 方法遮蔽掉** —— 所以这里必须显式写成 `HasWindowHandle::window_handle`。
pub fn hwnd_of(window: &gpui::Window) -> Option<*mut core::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as *mut core::ffi::c_void),
        _ => None,
    }
}

/// 窗口的外框矩形（**物理像素** —— 外壳是 DPI-aware 进程，拿到的就是物理像素）。
pub fn window_rect(hwnd: *mut core::ffi::c_void) -> Option<Rect> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
    let mut r: RECT = unsafe { std::mem::zeroed() };
    // SAFETY: hwnd 来自 gpui 的窗口句柄；GetWindowRect 只写这个栈上结构。
    let ok = unsafe { GetWindowRect(hwnd, &mut r) };
    (ok != 0).then(|| Rect {
        x: r.left,
        y: r.top,
        w: r.right - r.left,
        h: r.bottom - r.top,
    })
}

/// 窗口所在显示器的矩形，以及它是不是主显示器。
///
/// 为什么要知道「是不是主屏」：gdigrab 的 `desktop` 以**主显示器**左上角为原点，
/// 副屏上的窗口坐标会是负的 —— 那种情况我们直接拒绝，别录出一片黑。
pub fn monitor_rect(hwnd: *mut core::ffi::c_void) -> Option<(Rect, bool)> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
    let hwnd = hwnd as HWND;
    // SAFETY: 都是只读的 Win32 查询。
    let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if mon.is_null() {
        return None;
    }
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    let ok = unsafe { GetMonitorInfoW(mon, &mut info) };
    if ok == 0 {
        return None;
    }
    let r = info.rcMonitor;
    Some((
        Rect {
            x: r.left,
            y: r.top,
            w: r.right - r.left,
            h: r.bottom - r.top,
        },
        info.dwFlags & MONITORINFOF_PRIMARY != 0,
    ))
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn crop_region_inside_monitor_is_unchanged() {
        let got = crop_region(r(100, 80, 800, 600), r(0, 0, 2560, 1600), MIN_SIDE).unwrap();
        assert_eq!(got, r(100, 80, 800, 600));
    }

    #[test]
    fn crop_region_clips_to_monitor() {
        // 窗口比屏幕大：左边与上边在屏幕外
        let got = crop_region(r(-40, -30, 900, 700), r(0, 0, 800, 600), MIN_SIDE).unwrap();
        assert_eq!(got, r(0, 0, 800, 600));
    }

    #[test]
    fn crop_region_clips_bottom_right() {
        // 实测过的真实情况：窗口 (235,202) 2122x1406，屏幕只有 1600 高
        let got = crop_region(r(235, 202, 2122, 1406), r(0, 0, 2560, 1600), MIN_SIDE).unwrap();
        assert_eq!(got, r(235, 202, 2122, 1398));
    }

    #[test]
    fn crop_region_makes_sides_even() {
        // 奇数宽高必须砍成偶数，否则 libx264 编不出 yuv420p
        let got = crop_region(r(11, 13, 801, 601), r(0, 0, 2560, 1600), MIN_SIDE).unwrap();
        assert_eq!((got.w % 2, got.h % 2), (0, 0));
        assert!(got.w <= 801 && got.h <= 601);
    }

    #[test]
    fn crop_region_rejects_too_small() {
        assert!(crop_region(r(0, 0, 40, 40), r(0, 0, 2560, 1600), MIN_SIDE).is_none());
        // 只露出一点点（窗口几乎全在屏幕外）
        assert!(crop_region(r(0, 0, 500, 500), r(490, 490, 2560, 1600), MIN_SIDE).is_none());
    }

    #[test]
    fn crop_region_off_monitor_is_none() {
        assert!(crop_region(r(5000, 5000, 800, 600), r(0, 0, 2560, 1600), MIN_SIDE).is_none());
    }

    #[test]
    fn parse_dshow_devices_reads_real_ffmpeg_output() {
        let real = "[in#0 @ 000001a3c36223c0] \"Integrated Webcam\" (video)\n\
                    [in#0 @ 000001a3c36223c0]   Alternative name \"@device_pnp_\\\\?\\usb#vid_0c45&pid_6a27\"\n\
                    [in#0 @ 000001a3c36223c0] \"麦克风 (Realtek(R) Audio)\" (audio)\n\
                    [in#0 @ 000001a3c36223c0]   Alternative name \"@device_cm_{33D9A762}\"\n";
        let d = parse_dshow_devices(real);
        assert_eq!(d.video, vec!["Integrated Webcam".to_string()]);
        assert_eq!(d.audio, vec!["麦克风 (Realtek(R) Audio)".to_string()]);
    }

    #[test]
    fn parse_dshow_devices_tolerates_junk() {
        assert_eq!(parse_dshow_devices(""), Devices::default());
        assert_eq!(
            parse_dshow_devices("dummy: Immediate exit requested"),
            Devices::default()
        );
        // 有 (audio) 但没有引号对：跳过而不是 panic
        assert_eq!(parse_dshow_devices("[in#0] (audio)"), Devices::default());
    }

    #[test]
    fn screen_args_region_and_audio() {
        let a = screen_args(
            r(235, 202, 2122, 1398),
            Some("麦克风 (Realtek(R) Audio)"),
            30,
            Path::new("out.mp4"),
        );
        let s = a.join(" ");
        assert!(s.contains("-f gdigrab"));
        assert!(s.contains("-offset_x 235"));
        assert!(s.contains("-offset_y 202"));
        assert!(s.contains("-video_size 2122x1398"));
        assert!(s.contains("-i desktop"));
        // 中文设备名要原样进去（走 args 不过 shell，所以不需要引号）
        assert!(s.contains("-i audio=麦克风 (Realtek(R) Audio)"));
        assert!(s.contains("-c:v libx264"));
        assert!(s.contains("-map 0:v"));
        assert!(s.ends_with("out.mp4"));
    }

    #[test]
    fn screen_args_without_mic_has_no_audio_stream() {
        let a = screen_args(r(0, 0, 800, 600), None, 30, Path::new("out.mp4"));
        let s = a.join(" ");
        assert!(!s.contains("-c:a"));
        assert!(!s.contains("-map 1:a"));
        assert!(!s.contains("dshow"));
    }

    #[test]
    fn camera_args_asks_for_a_small_format() {
        let a = camera_args("Integrated Webcam", Path::new("c1.mp4"));
        let s = a.join(" ");
        assert!(s.contains("-video_size 640x480"));
        assert!(s.contains("-i video=Integrated Webcam"));
        assert!(!s.contains("-f dshow -i audio")); // 摄像头那一路不碰音频
    }

    #[test]
    fn concat_list_uses_forward_slashes_style_paths_verbatim() {
        let text = concat_list_text(&[PathBuf::from(r"D:\录屏\s1.mp4"), PathBuf::from("s2.mp4")]);
        assert_eq!(text, "file 'D:\\录屏\\s1.mp4'\nfile 's2.mp4'\n");
    }

    #[test]
    fn overlay_args_puts_pip_bottom_right() {
        let a = overlay_args(
            Path::new("s.mp4"),
            Path::new("c.mp4"),
            Path::new("o.mp4"),
            true,
        );
        let s = a.join(" ");
        assert!(s.contains("scale=320:-2"));
        assert!(s.contains("overlay=W-w-16:H-h-16"));
        // 摄像头先结束后面的画面还要继续出（不然画中画一断整段就没了）
        assert!(s.contains("eof_action=pass"));
        assert!(s.contains("-c:v h264_nvenc"));
        // 没声音也不能让整条命令失败
        assert!(s.contains("-map 0:a?"));
    }

    #[test]
    fn overlay_args_falls_back_to_x264() {
        let a = overlay_args(
            Path::new("s.mp4"),
            Path::new("c.mp4"),
            Path::new("o.mp4"),
            false,
        );
        assert!(a.join(" ").contains("-c:v libx264"));
    }

    #[test]
    fn elapsed_label_formats() {
        assert_eq!(elapsed_label(0), "00:00");
        assert_eq!(elapsed_label(9), "00:09");
        assert_eq!(elapsed_label(83), "01:23");
        assert_eq!(elapsed_label(3600), "1:00:00");
        assert_eq!(elapsed_label(3723), "1:02:03");
    }

    #[test]
    fn usable_needs_a_non_trivial_file() {
        let dir = std::env::temp_dir().join("typst-live-record-test");
        let _ = std::fs::create_dir_all(&dir);
        let tiny = dir.join("tiny.mp4");
        std::fs::write(&tiny, b"x").unwrap();
        assert!(!usable(&tiny));
        assert!(!usable(&dir.join("nope.mp4")));
        let big = dir.join("big.mp4");
        std::fs::write(&big, vec![0u8; 8192]).unwrap();
        assert!(usable(&big));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
