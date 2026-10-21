// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows lane's desk, as data: what a windowed run in the guest's
//! desktop session ([`crate::windows_window`]) reads and sends.
//!
//! - [`HELPER`], the PowerShell the run copies to [`HELPER_PATH`] and runs in
//!   the runner's desktop session (a windowless Task Scheduler task,
//!   [`crate::windows_guest::Desktop::Hidden`]): it reads a process's
//!   top-level windows (class, rectangle, client size, DPI, foreground,
//!   per-monitor awareness, the window's thread's keyboard layout and whether
//!   its IME is open), the monitors (name, rectangle, DPI, primary, display
//!   scaling, adapter), makes a monitor primary, sets a monitor's display
//!   scaling, and starts `aterm-gui` with its pid and exit status written down.
//!   Each answer is one line of `key=value` words.
//! - The QMP commands that type on the VM's USB keyboard and move and press
//!   its USB tablet: input as a person's hardware sends it, through Windows'
//!   own input stack (focus, IME, window manager), which no control verb of
//!   aterm's can stand in for.
//!
//! Everything here is text, so all of it is tested without a VM.

use std::fmt::Write as _;

use crate::windows_guest::{self as guest, cmd_quote};

/// Where a windowed run keeps its instances and [`HELPER`] in the guest.
pub const DESK: &str = r"C:\lane\desk";

/// Where [`HELPER`] is copied.
pub const HELPER_PATH: &str = r"C:\lane\desk\desk.ps1";

/// The desk helper (`desk.ps1 <verb> ...`), Windows PowerShell 5.1 with C#
/// compiled by `Add-Type`, per-monitor-v2 aware so every rectangle is in
/// physical pixels:
///
/// - `windows <pid>`: a `window ...` line per visible top-level window of
///   `pid`;
/// - `monitors`: a `monitor ...` line per monitor;
/// - `primary <name>`: that monitor primary at (0,0), every other to its
///   right, saved; then `monitors`;
/// - `scale <name> <steps>`: that monitor's display scaling set to `steps`
///   above its recommended one (Settings' list, 25 % apart from 100 %); then
///   `monitors`;
/// - `launch <bin-dir> <dir> <config-base64>`: `aterm-gui --control-sock
///   <dir>\aterm.sock -e powershell.exe -NoLogo -NoProfile` with
///   `XDG_CONFIG_HOME` and `ATERM_STATE_HOME` in `<dir>`, its stdout and
///   stderr kept there; its pid written to `<dir>\pid`, then its exit status
///   to `<dir>\exit` when it ends.
///
/// The display scaling is read and set through `DisplayConfigGetDeviceInfo`
/// and `DisplayConfigSetDeviceInfo` with the packet types Settings uses (-3
/// reads the steps a source offers and where it is, -4 sets it), which
/// Windows does not document.
pub const HELPER: &str = r##"$ErrorActionPreference = 'Stop'
$verb = $args[0]
if ($verb -eq 'launch') {
  $bin = $args[1]
  $d = $args[2]
  if (Test-Path $d) { Remove-Item -Recurse -Force $d }
  New-Item -ItemType Directory -Force -Path "$d\cfg\aterm", "$d\state" | Out-Null
  [IO.File]::WriteAllBytes("$d\cfg\aterm\aterm.toml", [Convert]::FromBase64String($args[3]))
  $env:XDG_CONFIG_HOME = "$d\cfg"
  $env:ATERM_STATE_HOME = "$d\state"
  $p = Start-Process -FilePath "$bin\aterm-gui.exe" -ArgumentList '--control-sock', "$d\aterm.sock", '-e', 'powershell.exe', '-NoLogo', '-NoProfile' -PassThru -RedirectStandardOutput "$d\stdout.log" -RedirectStandardError "$d\stderr.log"
  # Without its handle opened while it runs, the process object has no exit code.
  $null = $p.Handle
  Set-Content -Path "$d\pid" -Value $p.Id
  $p.WaitForExit()
  Set-Content -Path "$d\exit" -Value $p.ExitCode
  exit 0
}
$src = @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class LaneDesk {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct LUID { public uint Low; public int High; }
  [StructLayout(LayoutKind.Sequential)] public struct HDR { public int type; public int size; public LUID adapter; public uint id; }
  [StructLayout(LayoutKind.Sequential)] public struct DpiGet { public HDR h; public int min; public int cur; public int max; }
  [StructLayout(LayoutKind.Sequential)] public struct DpiSet { public HDR h; public int scale; }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct SrcName { public HDR h; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=32)] public string name; }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct MONITORINFOEX { public int cbSize; public RECT rcMonitor; public RECT rcWork; public uint dwFlags; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=32)] public string szDevice; }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct DISPLAY_DEVICE { public int cb; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=32)] public string DeviceName; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=128)] public string DeviceString; public int StateFlags; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=128)] public string DeviceID; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=128)] public string DeviceKey; }
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  public delegate bool MonProc(IntPtr m, IntPtr dc, ref RECT r, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
  [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr c);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] static extern uint GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern IntPtr GetWindowDpiAwarenessContext(IntPtr h);
  [DllImport("user32.dll")] static extern int GetAwarenessFromDpiAwarenessContext(IntPtr c);
  [DllImport("user32.dll")] static extern IntPtr GetKeyboardLayout(uint tid);
  [DllImport("imm32.dll")] static extern IntPtr ImmGetDefaultIMEWnd(IntPtr h);
  [DllImport("user32.dll")] static extern IntPtr SendMessageTimeoutW(IntPtr h, uint msg, IntPtr w, IntPtr l, uint flags, uint ms, out IntPtr result);
  [StructLayout(LayoutKind.Sequential)] public struct GUITHREADINFO { public int cbSize; public uint flags; public IntPtr hwndActive, hwndFocus, hwndCapture, hwndMenuOwner, hwndMoveSize, hwndCaret; public RECT rcCaret; }
  [DllImport("user32.dll")] static extern bool GetGUIThreadInfo(uint tid, ref GUITHREADINFO g);
  [DllImport("user32.dll")] static extern short GetAsyncKeyState(int vk);
  [DllImport("user32.dll")] static extern bool EnumDisplayMonitors(IntPtr dc, IntPtr clip, MonProc f, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern bool GetMonitorInfoW(IntPtr m, ref MONITORINFOEX mi);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern bool EnumDisplayDevicesW(string dev, uint i, ref DISPLAY_DEVICE d, uint flags);
  [DllImport("shcore.dll")] static extern int GetDpiForMonitor(IntPtr m, int t, out uint x, out uint y);
  [DllImport("user32.dll")] static extern int GetDisplayConfigBufferSizes(uint flags, out uint np, out uint nm);
  [DllImport("user32.dll")] static extern int QueryDisplayConfig(uint flags, ref uint np, [Out] byte[] paths, ref uint nm, [Out] byte[] modes, IntPtr topo);
  [DllImport("user32.dll")] static extern int SetDisplayConfig(uint np, byte[] paths, uint nm, byte[] modes, uint flags);
  [DllImport("user32.dll")] static extern int DisplayConfigGetDeviceInfo(ref DpiGet p);
  [DllImport("user32.dll")] static extern int DisplayConfigGetDeviceInfo(ref SrcName p);
  [DllImport("user32.dll")] static extern int DisplayConfigSetDeviceInfo(ref DpiSet p);
  [StructLayout(LayoutKind.Sequential)] public struct PMI { public ushort machine; public ushort res; public uint attrs; }
  [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
  [DllImport("kernel32.dll")] static extern bool GetProcessInformation(IntPtr h, int cls, ref PMI info, uint size);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  const int PATH = 72, MODE = 64;
  // QDC_ONLY_ACTIVE_PATHS; SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_SAVE_TO_DATABASE | SDC_ALLOW_CHANGES.
  const uint QDC = 2, SDC = 0x80 | 0x20 | 0x200 | 0x400;

  // The machine a process's code is for (ProcessMachineTypeInfo): AA64 native, 8664 x64 under Prism.
  static string MachineOf(uint pid) {
    var h = OpenProcess(0x1000, false, pid);
    if (h == IntPtr.Zero) return "-";
    var m = new PMI();
    bool ok = GetProcessInformation(h, 9, ref m, (uint)Marshal.SizeOf(typeof(PMI)));
    CloseHandle(h);
    return ok ? m.machine.ToString("x4") : "-";
  }

  public static string Windows(uint pid) {
    var sb = new StringBuilder();
    var machine = MachineOf(pid);
    var fg = GetForegroundWindow();
    EnumWindows((h, l) => {
      uint p; uint tid = GetWindowThreadProcessId(h, out p);
      if (p != pid || !IsWindowVisible(h)) return true;
      RECT r, c; GetWindowRect(h, out r); GetClientRect(h, out c);
      var cls = new StringBuilder(256); GetClassNameW(h, cls, 256);
      int open = Ime(h, 5);
      long layout = GetKeyboardLayout(tid).ToInt64() & 0xFFFF;
      sb.AppendFormat("window class={0} rect={1},{2},{3},{4} client={5}x{6} dpi={7} foreground={8} awareness={9} layout={10:x4} ime={11} machine={12} conv={13}\n",
        cls.ToString().Replace(' ', '_'), r.L, r.T, r.R, r.B, c.R, c.B, GetDpiForWindow(h), h == fg ? 1 : 0,
        GetAwarenessFromDpiAwarenessContext(GetWindowDpiAwarenessContext(h)), layout, open < 0 ? -1 : open == 0 ? 0 : 1, machine,
        Hex(Ime(h, 1)));
      return true; }, IntPtr.Zero);
    return sb.ToString();
  }

  // WM_IME_CONTROL to a window's thread's default IME window: IMC_GETOPENSTATUS (5) or IMC_GETCONVERSIONMODE (1); -1 unanswered.
  static int Ime(IntPtr h, int what) {
    var ime = ImmGetDefaultIMEWnd(h); IntPtr res;
    if (ime == IntPtr.Zero || SendMessageTimeoutW(ime, 0x283, (IntPtr)what, IntPtr.Zero, 2, 2000, out res) == IntPtr.Zero) return -1;
    return (int)res.ToInt64();
  }

  static string Hex(int v) { return v < 0 ? "-" : v.ToString("x"); }

  static IntPtr ClassWindow(uint pid, string cls) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
      uint p; GetWindowThreadProcessId(h, out p);
      if (p != pid || !IsWindowVisible(h)) return true;
      var c = new StringBuilder(256); GetClassNameW(h, c, 256);
      if (c.ToString() == cls) { found = h; return false; }
      return true; }, IntPtr.Zero);
    return found;
  }

  static System.Collections.Generic.Dictionary<uint, string> Exes = new System.Collections.Generic.Dictionary<uint, string>();
  static string Describe(IntPtr h) {
    if (h == IntPtr.Zero) return "0";
    uint p; GetWindowThreadProcessId(h, out p);
    var c = new StringBuilder(256); GetClassNameW(h, c, 256);
    string exe;
    if (!Exes.TryGetValue(p, out exe)) {
      exe = "?";
      try { exe = System.Diagnostics.Process.GetProcessById((int)p).ProcessName; } catch (Exception) { }
      Exes[p] = exe;
    }
    return h.ToInt64().ToString("x") + ":" + c.ToString().Replace(' ', '_') + ":" + exe + ":" + p;
  }

  // What the session's input does for `ms`, one line per change, to `path`:
  // the foreground window and its thread's focus, the window `cls` of `pid`'s
  // keyboard layout, IME open status and conversion mode, the keys held down,
  // and the visible top-level windows that come and go.
  public static void Watch(uint pid, string cls, int ms, string path) {
    var sw = System.Diagnostics.Stopwatch.StartNew();
    var seen = new System.Collections.Generic.HashSet<string>();
    bool first = true; string last = null; long nextWins = 0; IntPtr w = IntPtr.Zero;
    using (var f = new System.IO.StreamWriter(path, false, new UTF8Encoding(false))) {
      f.AutoFlush = true;
      while (sw.ElapsedMilliseconds < ms) {
        if (w == IntPtr.Zero) w = ClassWindow(pid, cls);
        var fg = GetForegroundWindow(); uint fpid; uint ftid = GetWindowThreadProcessId(fg, out fpid);
        var gi = new GUITHREADINFO(); gi.cbSize = Marshal.SizeOf(typeof(GUITHREADINFO));
        bool gok = ftid != 0 && GetGUIThreadInfo(ftid, ref gi);
        var keys = new StringBuilder();
        for (int vk = 1; vk < 0xFF; vk++) if ((GetAsyncKeyState(vk) & 0x8000) != 0) keys.Append(vk.ToString("x2")).Append(',');
        string state = "-";
        if (w != IntPtr.Zero) {
          uint p; uint tid = GetWindowThreadProcessId(w, out p);
          state = string.Format("layout={0:x4} ime={1} conv={2}", GetKeyboardLayout(tid).ToInt64() & 0xFFFF, Ime(w, 5), Hex(Ime(w, 1)));
        }
        var line = string.Format("fg={0} focus={1} flags={2:x} {3} keys={4}", Describe(fg), gok ? Describe(gi.hwndFocus) : "?", gi.flags, state, keys.ToString().TrimEnd(','));
        if (line != last) { f.WriteLine(sw.ElapsedMilliseconds + " " + line); last = line; }
        if (sw.ElapsedMilliseconds >= nextWins) {
          var now = new System.Collections.Generic.HashSet<string>();
          EnumWindows((h, l) => { if (IsWindowVisible(h)) now.Add(Describe(h)); return true; }, IntPtr.Zero);
          if (!first) {
            foreach (var s in now) if (!seen.Contains(s)) f.WriteLine(sw.ElapsedMilliseconds + " shown " + s);
            foreach (var s in seen) if (!now.Contains(s)) f.WriteLine(sw.ElapsedMilliseconds + " hidden " + s);
          }
          first = false; seen = now; nextWins = sw.ElapsedMilliseconds + 100;
        }
        System.Threading.Thread.Sleep(10);
      }
    }
  }

  static HDR Source(byte[] paths, int i) {
    var h = new HDR();
    h.adapter.Low = BitConverter.ToUInt32(paths, i * PATH); h.adapter.High = BitConverter.ToInt32(paths, i * PATH + 4);
    h.id = BitConverter.ToUInt32(paths, i * PATH + 8);
    return h;
  }
  static string NameOf(HDR h) {
    var n = new SrcName(); n.h = h; n.h.type = 1; n.h.size = Marshal.SizeOf(typeof(SrcName));
    return DisplayConfigGetDeviceInfo(ref n) == 0 ? n.name : "";
  }
  static void Query(out uint np, out uint nm, out byte[] paths, out byte[] modes) {
    int e = GetDisplayConfigBufferSizes(QDC, out np, out nm);
    if (e != 0) throw new Exception("GetDisplayConfigBufferSizes " + e);
    paths = new byte[np * PATH]; modes = new byte[nm * MODE];
    e = QueryDisplayConfig(QDC, ref np, paths, ref nm, modes, IntPtr.Zero);
    if (e != 0) throw new Exception("QueryDisplayConfig " + e);
  }
  static string ScaleOf(string name) {
    uint np, nm; byte[] paths, modes; Query(out np, out nm, out paths, out modes);
    for (int i = 0; i < np; i++) {
      var h = Source(paths, i);
      if (NameOf(h) != name) continue;
      var g = new DpiGet(); g.h = h; g.h.type = -3; g.h.size = Marshal.SizeOf(typeof(DpiGet));
      if (DisplayConfigGetDeviceInfo(ref g) != 0) return "-";
      return g.min + "/" + g.cur + "/" + g.max;
    }
    return "-";
  }
  static string AdapterOf(string name) {
    var d = new DISPLAY_DEVICE(); d.cb = Marshal.SizeOf(typeof(DISPLAY_DEVICE));
    for (uint i = 0; EnumDisplayDevicesW(null, i, ref d, 0); i++) {
      if (d.DeviceName == name) return d.DeviceString;
      d.cb = Marshal.SizeOf(typeof(DISPLAY_DEVICE));
    }
    return "-";
  }
  public static string Monitors() {
    var sb = new StringBuilder();
    EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero, (IntPtr m, IntPtr dc, ref RECT r, IntPtr l) => {
      var mi = new MONITORINFOEX(); mi.cbSize = Marshal.SizeOf(typeof(MONITORINFOEX));
      GetMonitorInfoW(m, ref mi);
      uint x, y; GetDpiForMonitor(m, 0, out x, out y);
      var b = mi.rcMonitor;
      sb.AppendFormat("monitor name={0} rect={1},{2},{3},{4} dpi={5} primary={6} scale={7} adapter={8}\n",
        mi.szDevice, b.L, b.T, b.R, b.B, x, mi.dwFlags & 1, ScaleOf(mi.szDevice), AdapterOf(mi.szDevice));
      return true; }, IntPtr.Zero);
    return sb.ToString();
  }

  public static string Primary(string primary) {
    uint np, nm; byte[] paths, modes; Query(out np, out nm, out paths, out modes);
    int prim = -1;
    for (int i = 0; i < np; i++) if (NameOf(Source(paths, i)) == primary) prim = i;
    if (prim < 0) return "error=no-source-named-" + primary;
    // Each active source's mode: width at 16, position at 28 and 32.
    int x = BitConverter.ToInt32(modes, (int)BitConverter.ToUInt32(paths, prim * PATH + 12) * MODE + 16);
    for (int i = 0; i < np; i++) {
      int at = (int)BitConverter.ToUInt32(paths, i * PATH + 12) * MODE;
      int px = 0;
      if (i != prim) { px = x; x += BitConverter.ToInt32(modes, at + 16); }
      Array.Copy(BitConverter.GetBytes(px), 0, modes, at + 28, 4);
      Array.Copy(BitConverter.GetBytes(0), 0, modes, at + 32, 4);
    }
    return "set=" + SetDisplayConfig(np, paths, nm, modes, SDC);
  }

  public static string Scale(string name, int steps) {
    uint np, nm; byte[] paths, modes; Query(out np, out nm, out paths, out modes);
    for (int i = 0; i < np; i++) {
      var h = Source(paths, i);
      if (NameOf(h) != name) continue;
      var s = new DpiSet(); s.h = h; s.h.type = -4; s.h.size = Marshal.SizeOf(typeof(DpiSet)); s.scale = steps;
      return "set=" + DisplayConfigSetDeviceInfo(ref s);
    }
    return "error=no-source-named-" + name;
  }
}
'@
# Compiled once per source: csc takes seconds, a probe runs many times.
$sha = [Security.Cryptography.SHA256]::Create()
$hash = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($src))).Replace('-', '').Substring(0, 16)
$dll = Join-Path $PSScriptRoot "LaneDesk-$hash.dll"
if (-not (Test-Path $dll)) { Add-Type -TypeDefinition $src -OutputAssembly $dll -OutputType Library }
Add-Type -Path $dll
[LaneDesk]::SetProcessDpiAwarenessContext([IntPtr]-4) | Out-Null
[LaneDesk]::SetThreadDpiAwarenessContext([IntPtr]-4) | Out-Null
switch ($verb) {
  'windows' { [LaneDesk]::Windows([uint32]$args[1]) }
  'monitors' { [LaneDesk]::Monitors() }
  'primary' { [LaneDesk]::Primary($args[1]); Start-Sleep -Seconds 2; [LaneDesk]::Monitors() }
  'scale' { [LaneDesk]::Scale($args[1], [int]$args[2]); Start-Sleep -Seconds 1; [LaneDesk]::Monitors() }
  'watch' { [LaneDesk]::Watch([uint32]$args[1], $args[2], [int]$args[3], $args[4]) }
  default { throw "desk.ps1: unknown verb '$verb'" }
}
"##;

/// The cmd.exe line that runs [`HELPER`] with `args`, stderr merged.
#[must_use]
pub fn helper_line(args: &[&str]) -> String {
    let words: Vec<String> = args.iter().map(|a| cmd_quote(a)).collect();
    format!(
        "powershell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File {HELPER_PATH} {} 2>&1",
        words.join(" ")
    )
}

/// A rectangle in physical pixels: left, top, right, bottom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rect {
    fn parse(text: &str) -> Option<Rect> {
        let mut it = text.split(',').map(|n| n.trim().parse::<i32>().ok());
        let rect = Rect {
            l: it.next()??,
            t: it.next()??,
            r: it.next()??,
            b: it.next()??,
        };
        it.next().is_none().then_some(rect)
    }

    #[must_use]
    pub fn width(&self) -> i32 {
        self.r - self.l
    }

    #[must_use]
    pub fn height(&self) -> i32 {
        self.b - self.t
    }

    /// Its centre.
    #[must_use]
    pub fn centre(&self) -> (i32, i32) {
        (self.l + self.width() / 2, self.t + self.height() / 2)
    }

    /// Whether `(x, y)` is inside it.
    #[must_use]
    pub fn holds(&self, (x, y): (i32, i32)) -> bool {
        (self.l..self.r).contains(&x) && (self.t..self.b).contains(&y)
    }
}

/// The word `key=…` of a helper line.
fn word<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// One `window` line of `desk.ps1 windows`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub class: String,
    pub rect: Rect,
    pub client: (u32, u32),
    pub dpi: u32,
    pub foreground: bool,
    /// `DPI_AWARENESS`: 2 is per-monitor aware.
    pub awareness: i32,
    /// The window's thread's keyboard layout's language (`0409` English,
    /// `0411` Japanese).
    pub layout: String,
    /// Whether its IME is open; `None` when the IME window did not answer.
    pub ime_open: Option<bool>,
    /// The machine its process's code is for (`IMAGE_FILE_MACHINE_*` in
    /// hex: `aa64`, or `8664` for x64 under Prism); `-` when unread.
    pub machine: String,
    /// Its IME's conversion mode (`IME_CMODE_*`); `None` when the IME window
    /// did not answer.
    pub conversion: Option<u32>,
}

/// The `window` lines of `desk.ps1 windows` (any other line is skipped).
#[must_use]
pub fn parse_windows(text: &str) -> Vec<Window> {
    text.lines()
        .filter(|l| l.starts_with("window "))
        .filter_map(|l| {
            let (w, h) = word(l, "client")?.split_once('x')?;
            Some(Window {
                class: word(l, "class")?.to_string(),
                rect: Rect::parse(word(l, "rect")?)?,
                client: (w.parse().ok()?, h.parse().ok()?),
                dpi: word(l, "dpi")?.parse().ok()?,
                foreground: word(l, "foreground")? == "1",
                awareness: word(l, "awareness")?.parse().ok()?,
                layout: word(l, "layout")?.to_string(),
                ime_open: match word(l, "ime")? {
                    "1" => Some(true),
                    "0" => Some(false),
                    _ => None,
                },
                machine: word(l, "machine")?.to_string(),
                conversion: match word(l, "conv")? {
                    "-" => None,
                    hex => Some(u32::from_str_radix(hex, 16).ok()?),
                },
            })
        })
        .collect()
}

/// The class aterm's window registers (winit's, named by aterm).
pub const ATERM_CLASS: &str = "aterm";

/// One `monitor` line of `desk.ps1 monitors`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    /// Its GDI device name (`\\.\DISPLAY2`).
    pub name: String,
    pub rect: Rect,
    pub dpi: u32,
    pub primary: bool,
    /// Its display scaling steps: the lowest, where it is, the highest (0
    /// is the recommended one); `None` when it offers none.
    pub scale: Option<(i32, i32, i32)>,
    /// Its adapter's name (`Red Hat VirtIO GPU DOD controller`).
    pub adapter: String,
}

/// The `monitor` lines of `desk.ps1 monitors` (any other line is skipped).
#[must_use]
pub fn parse_monitors(text: &str) -> Vec<Monitor> {
    text.lines()
        .filter(|l| l.starts_with("monitor "))
        .filter_map(|l| {
            let scale = word(l, "scale")?;
            let scale = if scale == "-" {
                None
            } else {
                let mut it = scale.split('/').map(|n| n.parse::<i32>().ok());
                Some((it.next()??, it.next()??, it.next()??))
            };
            Some(Monitor {
                name: word(l, "name")?.to_string(),
                rect: Rect::parse(word(l, "rect")?)?,
                dpi: word(l, "dpi")?.parse().ok()?,
                primary: word(l, "primary")? == "1",
                scale,
                adapter: l.split_once(" adapter=")?.1.trim().to_string(),
            })
        })
        .collect()
}

/// What the virtio-gpu's adapter is called once virtio-win's display driver
/// drives it.
pub const VIRTIO_ADAPTER: &str = "VirtIO GPU";

/// The USB keyboard's keys (QEMU qcodes) that type `c` on a US layout, the
/// modifier first; `None` for a character it has no key for.
#[must_use]
pub fn qcodes(c: char) -> Option<Vec<&'static str>> {
    const LETTERS: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    let at =
        |c: char, first: char| usize::try_from(u32::from(c).checked_sub(u32::from(first))?).ok();
    let letter = |c: char| LETTERS.get(at(c, 'a')?).copied();
    let plain = |k: &'static str| Some(vec![k]);
    let shifted = |k: &'static str| Some(vec!["shift", k]);
    match c {
        'a'..='z' => Some(vec![letter(c)?]),
        'A'..='Z' => Some(vec!["shift", letter(c.to_ascii_lowercase())?]),
        '0'..='9' => Some(vec![DIGITS.get(at(c, '0')?).copied()?]),
        ' ' => plain("spc"),
        '\n' => plain("ret"),
        '-' => plain("minus"),
        '=' => plain("equal"),
        '\'' => plain("apostrophe"),
        '[' => plain("bracket_left"),
        ']' => plain("bracket_right"),
        ';' => plain("semicolon"),
        ',' => plain("comma"),
        '.' => plain("dot"),
        '/' => plain("slash"),
        '\\' => plain("backslash"),
        '`' => plain("grave_accent"),
        '!' => shifted("1"),
        '@' => shifted("2"),
        '#' => shifted("3"),
        '$' => shifted("4"),
        '%' => shifted("5"),
        '^' => shifted("6"),
        '&' => shifted("7"),
        '*' => shifted("8"),
        '(' => shifted("9"),
        ')' => shifted("0"),
        '_' => shifted("minus"),
        '+' => shifted("equal"),
        '"' => shifted("apostrophe"),
        '{' => shifted("bracket_left"),
        '}' => shifted("bracket_right"),
        ':' => shifted("semicolon"),
        '<' => shifted("comma"),
        '>' => shifted("dot"),
        '?' => shifted("slash"),
        '|' => shifted("backslash"),
        '~' => shifted("grave_accent"),
        _ => None,
    }
}

/// QMP's `send-key` for the chord `keys` (qcodes, pressed in order, released
/// together after `hold_ms`).
#[must_use]
pub fn key_command(keys: &[&str], hold_ms: u32) -> String {
    let mut s = String::from("{\"execute\":\"send-key\",\"arguments\":{\"keys\":[");
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(s, "{{\"type\":\"qcode\",\"data\":\"{k}\"}}");
    }
    let _ = write!(s, "],\"hold-time\":{hold_ms}}}}}");
    s
}

/// The USB tablet's range on each axis: QEMU scales it to Windows' primary
/// monitor.
pub const TABLET_MAX: i64 = 32767;

/// QMP's `input-send-event` that moves the tablet to `(x, y)` on a primary
/// monitor `width` x `height` pixels (clamped to it).
#[must_use]
pub fn tablet_move(x: i32, y: i32, width: i32, height: i32) -> String {
    let axis = |v: i32, extent: i32| {
        let span = i64::from(extent.max(2) - 1);
        (i64::from(v.clamp(0, extent.max(2) - 1)) * TABLET_MAX) / span
    };
    format!(
        "{{\"execute\":\"input-send-event\",\"arguments\":{{\"events\":[\
         {{\"type\":\"abs\",\"data\":{{\"axis\":\"x\",\"value\":{}}}}},\
         {{\"type\":\"abs\",\"data\":{{\"axis\":\"y\",\"value\":{}}}}}]}}}}",
        axis(x, width),
        axis(y, height)
    )
}

/// QMP's `input-send-event` that presses (`down`) or releases the tablet's
/// left button.
#[must_use]
pub fn tablet_button(down: bool) -> String {
    format!(
        "{{\"execute\":\"input-send-event\",\"arguments\":{{\"events\":[\
         {{\"type\":\"btn\",\"data\":{{\"down\":{down},\"button\":\"left\"}}}}]}}}}"
    )
}

/// QMP's `screendump` of `device`'s console (the first, ramfb's, when
/// `None`) to `path` as PNG.
#[must_use]
pub fn screendump_command(path: &str, device: Option<&str>) -> String {
    let quoted = aterm_json::to_string(&aterm_json::Value::String(path.to_string()))
        .unwrap_or_else(|_| "\"\"".into());
    let device = device.map_or(String::new(), |d| format!(",\"device\":\"{d}\""));
    format!(
        "{{\"execute\":\"screendump\",\"arguments\":{{\"filename\":{quoted},\"format\":\"png\"{device}}}}}"
    )
}

/// The guest directory of instance `n` of `arch`'s windowed run.
#[must_use]
pub fn instance_dir(arch: guest::Arch, n: u32) -> String {
    format!("{DESK}\\{}-{n}", arch.name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helpers_windows_and_monitors_are_read_back() {
        let out = "window class=aterm rect=208,82,1346,1134 client=1116x996 dpi=144 foreground=1 awareness=2 layout=0411 ime=1 machine=8664 conv=19\r\n\
                   window class=Winit_Thread_Event_Target rect=0,0,16,16 client=0x0 dpi=96 foreground=0 awareness=2 layout=0409 ime=-1 machine=- conv=-\r\n\
                   window class=broken rect=1,2,3 client=1x1 dpi=96 foreground=0 awareness=2 layout=0409 ime=0 machine=aa64 conv=0\r\n";
        let w = parse_windows(out);
        assert_eq!(w.len(), 2, "{w:?}");
        assert_eq!(w[0].class, ATERM_CLASS);
        assert_eq!(
            w[0].rect,
            Rect {
                l: 208,
                t: 82,
                r: 1346,
                b: 1134
            }
        );
        assert_eq!(
            (w[0].client, w[0].dpi, w[0].foreground),
            ((1116, 996), 144, true)
        );
        assert_eq!(
            (w[0].awareness, w[0].layout.as_str(), w[0].ime_open),
            (2, "0411", Some(true))
        );
        assert_eq!(w[1].ime_open, None);
        assert_eq!(
            (w[0].machine.as_str(), w[1].machine.as_str()),
            ("8664", "-")
        );
        assert_eq!(w[0].rect.centre(), (777, 608));
        assert!(w[0].rect.holds((208, 82)) && !w[0].rect.holds((1346, 500)));

        let mons = "monitor name=\\\\.\\DISPLAY2 rect=0,0,1920,1200 dpi=96 primary=1 scale=0/0/4 adapter=Red Hat VirtIO GPU DOD controller\r\n\
                    monitor name=\\\\.\\DISPLAY1 rect=1920,0,2720,600 dpi=96 primary=0 scale=- adapter=Microsoft Basic Display Adapter\r\n\
                    set=0\r\n";
        let m = parse_monitors(mons);
        assert_eq!(m.len(), 2, "{m:?}");
        assert_eq!(m[0].name, "\\\\.\\DISPLAY2");
        assert!(m[0].primary && m[0].adapter.contains(VIRTIO_ADAPTER));
        assert_eq!(m[0].scale, Some((0, 0, 4)));
        assert_eq!(m[1].scale, None);
        assert_eq!(m[1].rect.width(), 800);
        assert_eq!(m[1].adapter, "Microsoft Basic Display Adapter");
    }

    #[test]
    fn every_character_typed_has_its_us_key_and_others_have_none() {
        assert_eq!(qcodes('a'), Some(vec!["a"]));
        assert_eq!(qcodes('W'), Some(vec!["shift", "w"]));
        assert_eq!(qcodes('7'), Some(vec!["7"]));
        assert_eq!(qcodes('('), Some(vec!["shift", "9"]));
        assert_eq!(qcodes('\''), Some(vec!["apostrophe"]));
        assert_eq!(qcodes('\n'), Some(vec!["ret"]));
        assert_eq!(qcodes('日'), None);
        assert_eq!(qcodes('é'), None);
        for c in "Write-Output ('IME-' + [Console]::WindowWidth*7)".chars() {
            assert!(qcodes(c).is_some(), "{c:?}");
        }
    }

    #[test]
    fn the_qmp_commands_are_json_qemu_reads() {
        let k = key_command(&["meta_l", "spc"], 60);
        assert_eq!(
            k,
            "{\"execute\":\"send-key\",\"arguments\":{\"keys\":[{\"type\":\"qcode\",\"data\":\"meta_l\"},\
             {\"type\":\"qcode\",\"data\":\"spc\"}],\"hold-time\":60}}"
        );
        for c in [
            k,
            tablet_move(959, 599, 1920, 1200),
            tablet_button(true),
            screendump_command("/tmp/a \"b\".png", Some("gpu1")),
            screendump_command("/tmp/a.png", None),
        ] {
            assert!(aterm_json::from_str::<aterm_json::Value>(&c).is_ok(), "{c}");
        }
        // The tablet spans the primary monitor: its last pixel is the range's end.
        assert!(tablet_move(0, 0, 1920, 1200).contains("\"value\":0}"));
        assert!(tablet_move(1919, 1199, 1920, 1200).contains(&format!("\"value\":{TABLET_MAX}}}")));
        assert!(
            tablet_move(5000, -3, 1920, 1200).contains(&format!("\"x\",\"value\":{TABLET_MAX}}}"))
        );
        assert!(screendump_command("/x.png", Some("gpu1")).contains(",\"device\":\"gpu1\""));
        assert!(!screendump_command("/x.png", None).contains("device"));
    }

    #[test]
    fn the_helper_runs_from_its_path_with_its_words_quoted() {
        let line = helper_line(&["scale", "\\\\.\\DISPLAY2", "2"]);
        assert!(line.starts_with("powershell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File C:\\lane\\desk\\desk.ps1 scale "));
        assert!(line.ends_with(" 2 2>&1"));
        assert_eq!(
            instance_dir(guest::Arch::X86_64, 2),
            "C:\\lane\\desk\\x86_64-2"
        );
        // Every verb the run sends is one the helper answers.
        for verb in ["'windows'", "'monitors'", "'primary'", "'scale'", "'watch'"] {
            assert!(HELPER.contains(&format!("  {verb} {{")), "{verb}");
        }
    }
}
