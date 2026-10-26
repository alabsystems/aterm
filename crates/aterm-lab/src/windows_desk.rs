// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows lane's desk, as data.

use std::fmt::Write as _;

use crate::windows_guest::{self as guest, cmd_quote};

/// Where a windowed run keeps its instances and [`HELPER`] in the guest.
pub const DESK: &str = r"C:\lane\desk";

/// Where [`HELPER`] is copied.
pub const HELPER_PATH: &str = r"C:\lane\desk\desk.ps1";

/// The desk helper (`desk.ps1 <verb> ...`), Windows PowerShell 5.1 with C#
/// compiled by `Add-Type`, per-monitor-v2 aware so every rectangle is in
/// physical pixels.
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
  # Words after the config are aterm-gui's own (`--cpu`), before `-e`.
  $extra = @()
  if ($args.Count -gt 4) { $extra = @($args[4..($args.Count - 1)]) }
  $argv = @('--control-sock', "$d\aterm.sock") + $extra + @('-e', 'powershell.exe', '-NoLogo', '-NoProfile')
  $p = Start-Process -FilePath "$bin\aterm-gui.exe" -ArgumentList $argv -PassThru -RedirectStandardOutput "$d\stdout.log" -RedirectStandardError "$d\stderr.log"
  # Without its handle opened while it runs, the process object has no exit code.
  $null = $p.Handle
  Set-Content -Path "$d\pid" -Value $p.Id
  $p.WaitForExit()
  Set-Content -Path "$d\exit" -Value $p.ExitCode
  exit 0
}
if ($verb -eq 'dragsource') {
  # A window whose label starts an OLE drag of one file (CF_HDROP) when the left button goes down
  # on it; where it is, and the drag's effect, go to the log. It is not DPI aware: the drop row
  # runs at 100 % scaling, where its coordinates are physical pixels.
  $file = $args[1]; $x = [int]$args[2]; $y = [int]$args[3]; $log = $args[4]
  Set-Content -Path $file -Value 'lane drop'
  Add-Type -AssemblyName System.Windows.Forms, System.Drawing
  $form = New-Object Windows.Forms.Form
  $form.Text = 'lane-drag'; $form.StartPosition = 'Manual'; $form.TopMost = $true
  $form.Location = New-Object Drawing.Point($x, $y); $form.Size = New-Object Drawing.Size(260, 160)
  $label = New-Object Windows.Forms.Label
  $label.Dock = 'Fill'; $label.Text = 'drag'; $label.TextAlign = 'MiddleCenter'
  $form.Controls.Add($label)
  # What the window under the cursor answered OLE's DragOver, and where the cursor was, one line
  # each time either changes: the drop row releases the button only once aterm's window has
  # answered Copy under it. A write the log refuses is tried again at the next answer.
  $script:over = ''
  $label.Add_GiveFeedback({
    param($s, $e)
    $p = [Windows.Forms.Cursor]::Position
    $line = "over effect={0} at={1},{2}" -f $e.Effect, $p.X, $p.Y
    if ($line -ne $script:over) {
      try { Add-Content -Path $log -Value $line; $script:over = $line } catch { }
    }
  })
  $label.Add_MouseDown({
    $data = New-Object Windows.Forms.DataObject([Windows.Forms.DataFormats]::FileDrop, [string[]]@($file))
    $effect = $label.DoDragDrop($data, [Windows.Forms.DragDropEffects]::Copy)
    Add-Content -Path $log -Value "dropped effect=$effect"
    $form.Close()
  })
  $form.Add_Shown({
    $p = $label.PointToScreen([Drawing.Point]::Empty)
    Add-Content -Path $log -Value ("source at={0},{1},{2},{3}" -f $p.X, $p.Y, ($p.X + $label.Width), ($p.Y + $label.Height))
  })
  [Windows.Forms.Application]::Run($form)
  exit 0
}
$src = @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class LaneDesk {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
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
  [DllImport("user32.dll")] static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] static extern int GetSystemMetrics(int i);
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
  [DllImport("user32.dll")] static extern IntPtr GetWindowLongPtrW(IntPtr h, int i);
  [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int attr, out int v, int size);
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
  [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleDC(IntPtr dc);
  [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleBitmap(IntPtr dc, int w, int h);
  [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr dc, IntPtr o);
  [DllImport("gdi32.dll")] static extern bool BitBlt(IntPtr dc, int x, int y, int w, int h, IntPtr src, int sx, int sy, uint rop);
  [StructLayout(LayoutKind.Sequential)] public struct BIH { public int size; public int w; public int h; public short planes; public short bits; public int comp; public int image; public int xppm; public int yppm; public int used; public int important; }
  [DllImport("gdi32.dll")] static extern int GetDIBits(IntPtr dc, IntPtr bmp, uint start, uint lines, [Out] byte[] bits, ref BIH info, uint usage);
  [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr o);
  [DllImport("gdi32.dll")] static extern bool DeleteDC(IntPtr dc);
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
      // GUI_INMOVESIZE: the window's thread is in Windows' modal move or size loop for this window.
      var gi = new GUITHREADINFO(); gi.cbSize = Marshal.SizeOf(typeof(GUITHREADINFO));
      bool sizing = GetGUIThreadInfo(tid, ref gi) && (gi.flags & 0x2) != 0 && gi.hwndMoveSize == h;
      // GWL_STYLE, GWL_EXSTYLE; DWMWA_SYSTEMBACKDROP_TYPE (38) and DWMWA_USE_IMMERSIVE_DARK_MODE (20), `-` unanswered.
      long style = GetWindowLongPtrW(h, -16).ToInt64() & 0xFFFFFFFF, exstyle = GetWindowLongPtrW(h, -20).ToInt64() & 0xFFFFFFFF;
      sb.AppendFormat("window class={0} rect={1},{2},{3},{4} client={5}x{6} dpi={7} foreground={8} awareness={9} layout={10:x4} ime={11} machine={12} conv={13} movesize={14} style={15:x8} exstyle={16:x8} backdrop={17} dark={18}\n",
        cls.ToString().Replace(' ', '_'), r.L, r.T, r.R, r.B, c.R, c.B, GetDpiForWindow(h), h == fg ? 1 : 0,
        GetAwarenessFromDpiAwarenessContext(GetWindowDpiAwarenessContext(h)), layout, open < 0 ? -1 : open == 0 ? 0 : 1, machine,
        Hex(Ime(h, 1)), sizing ? 1 : 0, style, exstyle, Dwm(h, 38), Dwm(h, 20));
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

  static string Dwm(IntPtr h, int attr) { int v; return DwmGetWindowAttribute(h, attr, out v, 4) == 0 ? v.ToString() : "-"; }

  // The client area of `pid`'s window of class `cls` as its DC holds it: copied
  // into a bitmap (BitBlt, SRCCOPY), read by GetDIBits as 32-bit top-down BI_RGB,
  // written to `path` as a binary PPM.
  public static string Readback(uint pid, string cls, string path) {
    var h = ClassWindow(pid, cls);
    if (h == IntPtr.Zero) return "error=no-window";
    RECT c; GetClientRect(h, out c);
    int w = c.R, ht = c.B;
    if (w <= 0 || ht <= 0) return "error=client-" + w + "x" + ht;
    var wdc = GetDC(h);
    var dc = CreateCompatibleDC(wdc);
    var bmp = CreateCompatibleBitmap(wdc, w, ht);
    var held = SelectObject(dc, bmp);
    bool blit = BitBlt(dc, 0, 0, w, ht, wdc, 0, 0, 0x00CC0020);
    SelectObject(dc, held);
    var info = new BIH(); info.size = Marshal.SizeOf(typeof(BIH)); info.w = w; info.h = -ht; info.planes = 1; info.bits = 32;
    var bits = new byte[w * ht * 4];
    int lines = GetDIBits(dc, bmp, 0, (uint)ht, bits, ref info, 0);
    DeleteObject(bmp); DeleteDC(dc); ReleaseDC(h, wdc);
    if (!blit || lines != ht) return "error=readback blit=" + blit + " lines=" + lines;
    var head = Encoding.ASCII.GetBytes("P6\n" + w + " " + ht + "\n255\n");
    var rgb = new byte[w * ht * 3];
    for (int i = 0; i < w * ht; i++) { rgb[i * 3] = bits[i * 4 + 2]; rgb[i * 3 + 1] = bits[i * 4 + 1]; rgb[i * 3 + 2] = bits[i * 4]; }
    using (var f = System.IO.File.Create(path)) { f.Write(head, 0, head.Length); f.Write(rgb, 0, rgb.Length); }
    return "readback " + Place(h);
  }

  // Where a window's client area is: its size, its top-left on the virtual screen,
  // and the virtual screen's rectangle (SM_X/Y/CX/CYVIRTUALSCREEN).
  static string Place(IntPtr h) {
    RECT c; GetClientRect(h, out c);
    var p = new POINT(); ClientToScreen(h, ref p);
    int x = GetSystemMetrics(76), y = GetSystemMetrics(77);
    return "client=" + c.R + "x" + c.B + " at=" + p.X + "," + p.Y + " screen=" + x + "," + y + "," + (x + GetSystemMetrics(78)) + "," + (y + GetSystemMetrics(79));
  }

  public static string Where(uint pid, string cls) {
    var h = ClassWindow(pid, cls);
    return h == IntPtr.Zero ? "error=no-window" : "place " + Place(h);
  }

  // Move `pid`'s window of class `cls` up or down so its client area's top is at
  // screen row `top`, its size and z-order kept, not activated.
  public static string Move(uint pid, string cls, int top) {
    var h = ClassWindow(pid, cls);
    return h == IntPtr.Zero ? "error=no-window" : MoveTo(h, top);
  }

  // Move `pid`'s window of class `cls` up until its client area's top `rows`
  // rows are above the virtual screen's top edge, as `move` does.
  public static string Lift(uint pid, string cls, int rows) {
    var h = ClassWindow(pid, cls);
    return h == IntPtr.Zero ? "error=no-window" : MoveTo(h, GetSystemMetrics(77) - rows);
  }

  static string MoveTo(IntPtr h, int top) {
    RECT r; GetWindowRect(h, out r);
    var p = new POINT(); ClientToScreen(h, ref p);
    // SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE.
    if (!SetWindowPos(h, IntPtr.Zero, r.L, r.T + top - p.Y, 0, 0, 0x1 | 0x4 | 0x10)) return "error=SetWindowPos";
    return "place " + Place(h);
  }

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
  'readback' { [LaneDesk]::Readback([uint32]$args[1], $args[2], $args[3]) }
  'place' { [LaneDesk]::Where([uint32]$args[1], $args[2]) }
  'move' { [LaneDesk]::Move([uint32]$args[1], $args[2], [int]$args[3]) }
  'lift' { [LaneDesk]::Lift([uint32]$args[1], $args[2], [int]$args[3]) }
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
    pub const fn width(&self) -> i32 {
        self.r - self.l
    }

    #[must_use]
    pub const fn height(&self) -> i32 {
        self.b - self.t
    }

    /// Its centre.
    #[must_use]
    pub const fn centre(&self) -> (i32, i32) {
        (self.l + self.width() / 2, self.t + self.height() / 2)
    }

    /// Whether `(x, y)` is inside it.
    #[must_use]
    pub fn holds(&self, (x, y): (i32, i32)) -> bool {
        (self.l..self.r).contains(&x) && (self.t..self.b).contains(&y)
    }
}

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
    /// The window's thread's keyboard layout's language (`0409` English, `0411` Japanese).
    pub layout: String,
    /// Whether its IME is open; `None` when the IME window did not answer.
    pub ime_open: Option<bool>,
    /// The machine its process's code is for; `-` when unread.
    pub machine: String,
    /// Its IME's conversion mode (`IME_CMODE_*`); `None` when the IME window did not answer.
    pub conversion: Option<u32>,
    /// Whether its thread is in Windows' modal move or size loop for it (`GUI_INMOVESIZE`).
    pub move_size: bool,
    /// Its `GWL_STYLE` and `GWL_EXSTYLE` words.
    pub style: u32,
    pub exstyle: u32,
    /// What DWM answers for its `DWMWA_SYSTEMBACKDROP_TYPE` and `DWMWA_USE_IMMERSIVE_DARK_MODE`;
    /// `None` when DWM did not answer.
    pub backdrop: Option<i32>,
    pub dark: Option<i32>,
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
                move_size: word(l, "movesize")? == "1",
                style: u32::from_str_radix(word(l, "style")?, 16).ok()?,
                exstyle: u32::from_str_radix(word(l, "exstyle")?, 16).ok()?,
                backdrop: word(l, "backdrop")?.parse().ok(),
                dark: word(l, "dark")?.parse().ok(),
            })
        })
        .collect()
}

/// Where a window's client area is, as a `readback`, `place` or `move` line says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    /// The client area's size.
    pub client: (usize, usize),
    /// Its top-left on the virtual screen.
    pub at: (i32, i32),
    /// The virtual screen: the rectangle every monitor lies in.
    pub screen: Rect,
}

impl Place {
    /// The part of the client area inside the virtual screen, in client coordinates (`l..r` columns, `t..b` rows), or `None` when none of it is.
    #[must_use]
    pub fn on_screen(&self) -> Option<Rect> {
        let (w, h) = (
            i32::try_from(self.client.0).ok()?,
            i32::try_from(self.client.1).ok()?,
        );
        let r = Rect {
            l: (self.screen.l - self.at.0).max(0),
            t: (self.screen.t - self.at.1).max(0),
            r: (self.screen.r - self.at.0).min(w),
            b: (self.screen.b - self.at.1).min(h),
        };
        (r.l < r.r && r.t < r.b).then_some(r)
    }

    /// The parts of the client area on `monitors` (their rectangles on the virtual screen), in
    /// client coordinates, one for each monitor the client area reaches, in `monitors`' order.
    /// Monitors of different sizes leave parts of the virtual screen on none of them (below the
    /// 800x600 second monitor, beside the 1920x1200 primary), and a client area there is on the
    /// virtual screen but not on these.
    #[must_use]
    pub fn on_monitors(&self, monitors: &[Rect]) -> Vec<Rect> {
        let (Ok(w), Ok(h)) = (i32::try_from(self.client.0), i32::try_from(self.client.1)) else {
            return Vec::new();
        };
        monitors
            .iter()
            .map(|m| Rect {
                l: (m.l - self.at.0).max(0),
                t: (m.t - self.at.1).max(0),
                r: (m.r - self.at.0).min(w),
                b: (m.b - self.at.1).min(h),
            })
            .filter(|r| r.l < r.r && r.t < r.b)
            .collect()
    }
}

/// The [`Place`] a `readback`, `place` or `move` line names.
#[must_use]
pub fn parse_place(text: &str) -> Option<Place> {
    let line = text
        .lines()
        .find(|l| l.starts_with("readback ") || l.starts_with("place "))?;
    let (w, h) = word(line, "client")?.split_once('x')?;
    let (x, y) = word(line, "at")?.split_once(',')?;
    Some(Place {
        client: (w.parse().ok()?, h.parse().ok()?),
        at: (x.parse().ok()?, y.parse().ok()?),
        screen: Rect::parse(word(line, "screen")?)?,
    })
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
    /// Its display scaling steps: the lowest, where it is, the highest; `None` when it offers none.
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

/// What the virtio-gpu's adapter is called once virtio-win's display driver drives it.
pub const VIRTIO_ADAPTER: &str = "VirtIO GPU";

/// The USB keyboard's keys (QEMU qcodes) that type `c` on a US layout, the modifier first.
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

/// QMP's `send-key` for the chord `keys`.
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

/// The USB tablet's range on each axis: QEMU scales it to Windows' primary monitor.
pub const TABLET_MAX: i64 = 32767;

/// QMP's `input-send-event` that moves the tablet to `(x, y)` on a primary monitor `width` x `height` pixels (clamped to it).
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

/// QMP's `input-send-event` that presses (`down`) or releases the tablet's left button.
#[must_use]
pub fn tablet_button(down: bool) -> String {
    format!(
        "{{\"execute\":\"input-send-event\",\"arguments\":{{\"events\":[\
         {{\"type\":\"btn\",\"data\":{{\"down\":{down},\"button\":\"left\"}}}}]}}}}"
    )
}

/// QMP's `screendump` of `device`'s console (the first, ramfb's, when `None`) to `path` as PNG.
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
        let out = "window class=aterm rect=208,82,1346,1134 client=1116x996 dpi=144 foreground=1 awareness=2 layout=0411 ime=1 machine=8664 conv=19 movesize=1 style=16cf0000 exstyle=00200100 backdrop=0 dark=1\r\n\
                   window class=Winit_Thread_Event_Target rect=0,0,16,16 client=0x0 dpi=96 foreground=0 awareness=2 layout=0409 ime=-1 machine=- conv=- movesize=0 style=94000000 exstyle=00000080 backdrop=- dark=-\r\n\
                   window class=broken rect=1,2,3 client=1x1 dpi=96 foreground=0 awareness=2 layout=0409 ime=0 machine=aa64 conv=0 movesize=0 style=0 exstyle=0 backdrop=0 dark=0\r\n\
                   window class=aterm rect=0,0,9,9 client=9x9 dpi=96 foreground=0 awareness=2 layout=0407 ime=0 machine=aa64 conv=0 movesize=0\r\n";
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
        assert_eq!((w[0].move_size, w[1].move_size), (true, false));
        assert_eq!(
            (w[0].style, w[0].exstyle, w[0].backdrop, w[0].dark),
            (0x16cf_0000, 0x0020_0100, Some(0), Some(1))
        );
        assert_eq!((w[1].backdrop, w[1].dark), (None, None));
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

        assert_eq!(parse_place("error=no-window\r\n"), None);
        let cut = parse_place("readback client=1076x939 at=239,273 screen=0,0,2720,1200\r\n")
            .expect("a place");
        assert_eq!(cut.client, (1076, 939));
        assert_eq!(cut.at, (239, 273));
        assert_eq!(
            cut.on_screen(),
            Some(Rect {
                l: 0,
                t: 0,
                r: 1076,
                b: 927
            })
        );
        let whole = parse_place("place client=1076x939 at=239,261 screen=0,0,2720,1200\r\n")
            .expect("a place");
        assert_eq!(
            whole.on_screen(),
            Some(Rect {
                l: 0,
                t: 0,
                r: 1076,
                b: 939
            })
        );
        let left = parse_place("place client=100x50 at=-30,-10 screen=0,0,1920,1200")
            .and_then(|p| p.on_screen());
        assert_eq!(
            left,
            Some(Rect {
                l: 30,
                t: 10,
                r: 100,
                b: 50
            })
        );
        assert_eq!(
            parse_place("place client=100x50 at=3000,0 screen=0,0,1920,1200")
                .and_then(|p| p.on_screen()),
            None
        );
        assert_eq!(parse_place("readback client=1116x996\r\n"), None);

        // The lane's monitors, and a 150 % client area back on the primary but reaching past its
        // right edge: on the virtual screen whole, on the monitors but for the part below the
        // second one.
        let monitors: Vec<Rect> = m.iter().map(|m| m.rect).collect();
        let across = parse_place("readback client=1080x903 at=1204,45 screen=0,0,2720,1200")
            .expect("a place");
        assert_eq!(
            across.on_screen(),
            Some(Rect {
                l: 0,
                t: 0,
                r: 1080,
                b: 903
            })
        );
        assert_eq!(
            across.on_monitors(&monitors),
            vec![
                Rect {
                    l: 0,
                    t: 0,
                    r: 716,
                    b: 903
                },
                Rect {
                    l: 716,
                    t: 0,
                    r: 1080,
                    b: 555
                }
            ]
        );
        assert_eq!(
            cut.on_monitors(&monitors),
            vec![cut.on_screen().expect("a part")]
        );
        assert_eq!(
            parse_place("place client=100x50 at=2000,700 screen=0,0,2720,1200")
                .expect("a place")
                .on_monitors(&monitors),
            Vec::new(),
            "below the second monitor, beside the primary"
        );
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
        for verb in [
            "'windows'",
            "'monitors'",
            "'primary'",
            "'scale'",
            "'watch'",
            "'readback'",
            "'place'",
            "'move'",
            "'lift'",
        ] {
            assert!(HELPER.contains(&format!("  {verb} {{")), "{verb}");
        }
        for verb in ["'launch'", "'dragsource'"] {
            assert!(
                HELPER.contains(&format!("if ($verb -eq {verb}) {{")),
                "{verb}"
            );
        }
    }
}
