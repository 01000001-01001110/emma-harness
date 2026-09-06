//! The Windows backend: GDI for the pixels, GDI+ for the two files.
//!
//! **Why this is not a PowerShell script** is argued in the crate's module
//! doc and is not repeated here; the short form is that AMSI blocks the
//! capture-resize-encode shape and the only ways round that are evasion.
//!
//! **Why GDI and not the Desktop Duplication API.** `BitBlt` from the screen
//! DC is the oldest and most boring way to read the desktop, it needs no
//! device, no COM apartment and no acquire/release loop, and it works on a
//! remote-desktop session where duplication returns `DXGI_ERROR_UNSUPPORTED`.
//! Duplication is faster per frame and this takes one frame every few minutes.
//!
//! **What every function here is careful about.** Each GDI and GDI+ handle is
//! owned by a guard whose `Drop` releases it, because the error paths are the
//! common ones — a capture that fails is a capture on a locked session, and
//! leaking a DC per attempt in a long-running agent is the kind of slow failure
//! nobody attributes to a screenshot tool.
//!
//! # The two things this cannot see
//!
//! A process on a non-interactive window station blits a black rectangle of the
//! right size and every call here returns success. There is no status that says
//! otherwise, which is why the crate doc names it beside the macOS wallpaper
//! trap rather than pretending only one platform has one.
//!
//! And this process is not DPI-aware, so on a scaled display the monitor
//! rectangles and the blit are the virtualised size rather than the panel's.
//! That is a property of the process, not of this module: making it otherwise
//! means `SetProcessDpiAwarenessContext` at start-up, which is the binary's
//! decision and would change how every other window in the process is measured.
//! The description says the dimensions may be smaller than the panel.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};

use emma_tool_api::ToolError;
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    EnumDisplayMonitors, GetDC, GetMonitorInfoW, ReleaseDC, SelectObject, SetBrushOrgEx,
    SetStretchBltMode, StretchBlt, CAPTUREBLT, HALFTONE, HBITMAP, HDC, HMONITOR, MONITORINFO,
    SRCCOPY,
};
use windows_sys::Win32::Graphics::GdiPlus::{
    EncoderParameter, EncoderParameterValueTypeLong, EncoderParameters, EncoderQuality,
    GdipCreateBitmapFromHBITMAP, GdipDisposeImage, GdipGetImageEncoders, GdipGetImageEncodersSize,
    GdipSaveImageToFile, GdiplusShutdown, GdiplusStartup, GdiplusStartupInput,
    GdiplusStartupOutput, GpBitmap, GpImage, ImageCodecInfo, Ok as GpOk,
};

use super::{bounded_dimensions, Backend, Capture, Shot};

/// `MONITORINFOF_PRIMARY`, which `windows-sys` does not define. One bit, and
/// the reason it is needed at all: `EnumDisplayMonitors` hands monitors back in
/// the order the display driver enumerates them, which is not the order a human
/// would call "the first one".
const MONITORINFOF_PRIMARY: u32 = 1;

/// JPEG quality for the bounded copy, as a percentage.
///
/// GDI+ defaults to 75, which is visibly soft on exactly the thing a screenshot
/// exists to show — a menu label, a stack trace, a dialog's small print. 85 is
/// where the size stops being worth the further sharpness.
const JPEG_QUALITY: u32 = 85;

/// The Windows backend. Constructed by [`super::host_capture`].
pub struct Gdi;

impl Capture for Gdi {
    fn backend(&self) -> Backend {
        Backend::Gdi
    }

    fn shoot(
        &self,
        shot: &Shot,
        full: &Path,
        bounded: Option<(&Path, u32)>,
    ) -> Result<Option<String>, ToolError> {
        let (x, y, w, h) = target_rect(shot)?;
        // Guarded before the blit rather than after: `CreateCompatibleBitmap`
        // with a zero dimension returns a null handle and everything downstream
        // then fails with a status code that says nothing about why.
        if w <= 0 || h <= 0 {
            return Err(ToolError::BadArguments(format!(
                "{} is {w}x{h} pixels, which is not an area that can be captured",
                shot.what()
            )));
        }

        let screen = ScreenDc::open()?;
        let mem = MemDc::compatible(screen.0)?;
        let bmp = Bmp::compatible(screen.0, w, h)?;
        // SAFETY: `mem` is a memory DC this function owns and `bmp` a bitmap
        // compatible with the screen DC it was made from. The previously
        // selected object is the 1x1 monochrome default, which needs no
        // restoring before `DeleteDC`.
        unsafe { SelectObject(mem.0, bmp.0) };
        // CAPTUREBLT so layered windows — which is most menus, tooltips and
        // anything with a shadow — are in the picture. Without it they are
        // holes, and a screenshot with the menu missing is worse than none.
        //
        // SAFETY: both DCs are live and the rectangle is inside the virtual
        // screen by construction of `target_rect`.
        let ok = unsafe { BitBlt(mem.0, 0, 0, w, h, screen.0, x, y, SRCCOPY | CAPTUREBLT) };
        if ok == 0 {
            return Err(ToolError::Failed(format!(
                "the screen could not be copied for {} (BitBlt failed: {}). {}",
                shot.what(),
                last_error(),
                Backend::Gdi.hint()
            )));
        }

        let _gp = GdiPlus::start()?;
        let png = encoder("image/png").ok_or_else(|| {
            ToolError::Unavailable(
                "this machine's GDI+ has no PNG encoder, which no supported Windows should \
                 be missing"
                    .to_string(),
            )
        })?;
        let image = Image::from_bitmap(bmp.0)?;
        image.save(full, &png, None)?;

        let Some((dest, max_dim)) = bounded else {
            return Ok(None);
        };
        // From here on a failure is a missing bounded copy rather than a
        // missing capture, so it is reported as words in the result and not as
        // an error: the picture on disk is already good.
        Ok(self.bounded_copy(&screen, &mem, w, h, dest, max_dim).err())
    }
}

impl Gdi {
    /// The bounded copy: one `StretchBlt` and one GDI+ encode.
    ///
    /// **`from` is the DC the capture is already selected into, and that is not
    /// a convenience.** A bitmap may be selected into exactly one device
    /// context at a time; selecting it into a second returns null and leaves
    /// that DC holding its default 1x1 monochrome bitmap. The `StretchBlt` then
    /// succeeds, the JPEG is valid, and it is solid black. That was written the
    /// other way first and every assertion in the suite passed on it — the PNG
    /// signature, the dimensions, the JPEG magic, the copy being smaller. Only
    /// looking at the picture found it.
    ///
    /// `HALFTONE` rather than the default `BLACKONWHITE`, which for a downscale
    /// keeps one source pixel out of every n and turns a line of text into
    /// noise. `SetBrushOrgEx` after it is not optional decoration — the
    /// documentation requires it and omitting it leaves brush alignment
    /// artefacts on some drivers.
    fn bounded_copy(
        &self,
        screen: &ScreenDc,
        from: &MemDc,
        w: i32,
        h: i32,
        dest: &Path,
        max_dim: u32,
    ) -> Result<(), String> {
        let (bw, bh) = bounded_dimensions(w, h, max_dim);
        let to = MemDc::compatible(screen.0).map_err(|e| e.detail().to_string())?;
        let small = Bmp::compatible(screen.0, bw, bh).map_err(|e| e.detail().to_string())?;
        // SAFETY: two live DCs and one live bitmap, all owned by this call or
        // its caller.
        let ok = unsafe {
            SelectObject(to.0, small.0);
            SetStretchBltMode(to.0, HALFTONE);
            SetBrushOrgEx(to.0, 0, 0, null_mut());
            StretchBlt(to.0, 0, 0, bw, bh, from.0, 0, 0, w, h, SRCCOPY)
        };
        if ok == 0 {
            return Err(format!(
                "the capture could not be resized to {bw}x{bh} (StretchBlt failed: {})",
                last_error()
            ));
        }
        let jpeg = encoder("image/jpeg")
            .ok_or_else(|| "this machine's GDI+ has no JPEG encoder".to_string())?;
        let image = Image::from_bitmap(small.0).map_err(|e| e.detail().to_string())?;
        let quality = JPEG_QUALITY;
        let param = EncoderParameter {
            Guid: EncoderQuality,
            NumberOfValues: 1,
            Type: EncoderParameterValueTypeLong as u32,
            Value: (&quality as *const u32 as *mut std::ffi::c_void),
        };
        let params = EncoderParameters {
            Count: 1,
            Parameter: [param],
        };
        image
            .save(dest, &jpeg, Some(&params))
            .map_err(|e| e.detail().to_string())
    }
}

/// The rectangle to capture, in virtual-screen coordinates.
fn target_rect(shot: &Shot) -> Result<(i32, i32, i32, i32), ToolError> {
    if let Some((x, y, w, h)) = shot.region {
        return Ok((x as i32, y as i32, w as i32, h as i32));
    }
    let monitors = monitors();
    if monitors.is_empty() {
        return Err(ToolError::Unavailable(
            "Windows reported no displays, which is what a session with no desktop looks like \
             from here."
                .to_string(),
        ));
    }
    let n = shot.display as usize;
    let r = monitors.get(n - 1).ok_or_else(|| {
        ToolError::BadArguments(format!(
            "display {n} does not exist: this machine has {}",
            monitors.len()
        ))
    })?;
    Ok((r.left, r.top, r.right - r.left, r.bottom - r.top))
}

/// Every display's rectangle, primary first.
///
/// Primary first because `display: 1` is documented as the main display and
/// `EnumDisplayMonitors` order is the driver's, which follows however the user
/// arranged the monitors in Settings. A stable sort keeps the rest in
/// enumeration order, so `display: 2` does not move around between calls.
fn monitors() -> Vec<RECT> {
    let mut found: Vec<(bool, RECT)> = Vec::new();
    // SAFETY: the callback is `collect` below, which treats `data` as the
    // `&mut Vec` this passes and touches nothing else; the vector outlives the
    // call because `EnumDisplayMonitors` is synchronous.
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect),
            &mut found as *mut Vec<(bool, RECT)> as LPARAM,
        );
    }
    found.sort_by_key(|(primary, _)| !primary);
    found.into_iter().map(|(_, r)| r).collect()
}

/// `EnumDisplayMonitors`' callback. See [`monitors`] for the safety argument.
unsafe extern "system" fn collect(h: HMONITOR, _dc: HDC, _clip: *mut RECT, data: LPARAM) -> BOOL {
    let out = &mut *(data as *mut Vec<(bool, RECT)>);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(h, &mut info) != 0 {
        out.push((info.dwFlags & MONITORINFOF_PRIMARY != 0, info.rcMonitor));
    }
    // Keep enumerating: a monitor whose info could not be read is skipped
    // rather than ending the walk, so one odd display does not hide the rest.
    1
}

/// The last Win32 error, as a number.
///
/// A number rather than `FormatMessageW`'s sentence, deliberately: the sentence
/// needs a language pack, a buffer and a second failure path, and the code is
/// what a reader searches for anyway.
fn last_error() -> u32 {
    // SAFETY: no arguments, no state.
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

// ---------------------------------------------------------------------------
// Handle guards
//
// One per handle type. Every one of these is dropped on the error paths as
// well as the success path, which is the whole reason they exist.
// ---------------------------------------------------------------------------

/// The screen's device context, released on drop.
struct ScreenDc(HDC);

impl ScreenDc {
    fn open() -> Result<Self, ToolError> {
        // SAFETY: a null window handle asks for the whole virtual screen, which
        // is what this tool captures.
        let dc = unsafe { GetDC(null_mut()) };
        if dc.is_null() {
            return Err(ToolError::Unavailable(format!(
                "Windows would not hand out a device context for the screen (error {}). {}",
                last_error(),
                Backend::Gdi.hint()
            )));
        }
        Ok(Self(dc))
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: the handle came from `GetDC(null)` and is released once.
        unsafe { ReleaseDC(null_mut(), self.0) };
    }
}

/// A memory device context, deleted on drop.
struct MemDc(HDC);

impl MemDc {
    fn compatible(dc: HDC) -> Result<Self, ToolError> {
        // SAFETY: `dc` is a live DC owned by the caller.
        let mem = unsafe { CreateCompatibleDC(dc) };
        if mem.is_null() {
            return Err(ToolError::Failed(format!(
                "a memory device context could not be created (error {})",
                last_error()
            )));
        }
        Ok(Self(mem))
    }
}

impl Drop for MemDc {
    fn drop(&mut self) {
        // SAFETY: created by `CreateCompatibleDC` and deleted once.
        unsafe { DeleteDC(self.0) };
    }
}

/// A device-dependent bitmap, deleted on drop.
struct Bmp(HBITMAP);

impl Bmp {
    fn compatible(dc: HDC, w: i32, h: i32) -> Result<Self, ToolError> {
        // SAFETY: `dc` is a live screen DC; `w` and `h` are checked positive by
        // the caller.
        let bmp = unsafe { CreateCompatibleBitmap(dc, w, h) };
        if bmp.is_null() {
            return Err(ToolError::Failed(format!(
                "a {w}x{h} bitmap could not be created (error {}). A capture that large may \
                 simply not fit in this session's GDI budget.",
                last_error()
            )));
        }
        Ok(Self(bmp))
    }
}

impl Drop for Bmp {
    fn drop(&mut self) {
        // SAFETY: created by `CreateCompatibleBitmap` and deleted once. It is
        // still selected into a memory DC in the success path, which is
        // permitted: the DC's own `Drop` runs first because it is declared
        // first, and a bitmap selected into a deleted DC is no longer selected.
        unsafe { DeleteObject(self.0 as _) };
    }
}

/// GDI+ initialised for the length of one capture.
///
/// Started and shut down per call rather than once per process. It is a few
/// hundred microseconds against a capture measured in tens of milliseconds, and
/// the alternative is a process-wide token this crate would own on behalf of a
/// binary that never asked for one.
struct GdiPlus(usize);

impl GdiPlus {
    fn start() -> Result<Self, ToolError> {
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: 0,
            SuppressExternalCodecs: 0,
        };
        let mut token: usize = 0;
        let mut output: GdiplusStartupOutput = GdiplusStartupOutput {
            NotificationHook: 0,
            NotificationUnhook: 0,
        };
        // SAFETY: both structures are initialised above and outlive the call.
        let status = unsafe { GdiplusStartup(&mut token, &input, &mut output) };
        if status != GpOk {
            return Err(ToolError::Unavailable(format!(
                "GDI+ would not start (status {status}), so the capture cannot be encoded"
            )));
        }
        Ok(Self(token))
    }
}

impl Drop for GdiPlus {
    fn drop(&mut self) {
        // SAFETY: the token came from a successful `GdiplusStartup`.
        unsafe { GdiplusShutdown(self.0) };
    }
}

/// A GDI+ image, disposed on drop.
struct Image(*mut GpImage);

impl Image {
    /// Wrap a GDI bitmap. GDI+ copies the pixels, so the source may be deleted
    /// afterwards — which is what the guards above do, in declaration order.
    fn from_bitmap(bmp: HBITMAP) -> Result<Self, ToolError> {
        let mut out: *mut GpBitmap = null_mut();
        // SAFETY: `bmp` is a live bitmap; a null palette means "use the
        // bitmap's own colours", which a compatible bitmap has.
        let status = unsafe { GdipCreateBitmapFromHBITMAP(bmp, null_mut(), &mut out) };
        if status != GpOk || out.is_null() {
            return Err(ToolError::Failed(format!(
                "the capture could not be handed to GDI+ (status {status})"
            )));
        }
        Ok(Self(out.cast()))
    }

    fn save(
        &self,
        path: &Path,
        clsid: &GUID,
        params: Option<&EncoderParameters>,
    ) -> Result<(), ToolError> {
        let wide = wide(path);
        // SAFETY: `wide` is NUL-terminated and outlives the call; `clsid` came
        // from GDI+'s own encoder list.
        let status = unsafe {
            GdipSaveImageToFile(
                self.0,
                wide.as_ptr(),
                clsid,
                params.map_or(null(), |p| p as *const EncoderParameters),
            )
        };
        if status != GpOk {
            return Err(ToolError::Failed(format!(
                "the capture could not be written to {} (GDI+ status {status})",
                path.display()
            )));
        }
        Ok(())
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        // SAFETY: created by GDI+ and disposed once, while GDI+ is still up
        // because the `GdiPlus` guard is declared before every `Image`.
        unsafe { GdipDisposeImage(self.0) };
    }
}

/// The CLSID of the installed encoder for a MIME type.
///
/// Looked up rather than hard-coded. The GUIDs are stable and published, and
/// hard-coding them would still be wrong: what is being asked is whether *this*
/// machine has that encoder, and a constant cannot answer that.
fn encoder(mime: &str) -> Option<GUID> {
    let (mut count, mut size) = (0u32, 0u32);
    // SAFETY: both out-parameters are live.
    let status = unsafe { GdipGetImageEncodersSize(&mut count, &mut size) };
    if status != GpOk || size == 0 || count == 0 {
        return None;
    }
    // GDI+ writes `count` `ImageCodecInfo` structures plus the strings they
    // point at into one buffer of `size` bytes, so the buffer has to be that
    // large and not `count * size_of::<ImageCodecInfo>()`.
    let mut buf = vec![0u8; size as usize];
    // SAFETY: the buffer is exactly the size GDI+ just asked for.
    let status = unsafe { GdipGetImageEncoders(count, size, buf.as_mut_ptr().cast()) };
    if status != GpOk {
        return None;
    }
    // SAFETY: the first `count` structures of the buffer are `ImageCodecInfo`,
    // as documented, and the buffer outlives the slice.
    let infos = unsafe {
        std::slice::from_raw_parts(buf.as_ptr() as *const ImageCodecInfo, count as usize)
    };
    infos
        .iter()
        .find(|info| wide_is(info.MimeType, mime))
        .map(|info| info.Clsid)
}

/// Whether a NUL-terminated wide string equals this ASCII one.
///
/// Written out rather than reached for through `OsString::from_wide` because
/// that needs the length first, which means walking the string twice for a
/// comparison that can stop at the first difference.
fn wide_is(p: *const u16, ascii: &str) -> bool {
    if p.is_null() {
        return false;
    }
    for (i, b) in ascii.bytes().enumerate() {
        // SAFETY: the string is NUL-terminated, so the walk stops at or before
        // the terminator — the mismatch on `0` returns before reading past it.
        let c = unsafe { *p.add(i) };
        if c != u16::from(b) {
            return false;
        }
    }
    // SAFETY: as above; index `ascii.len()` is at most the terminator.
    unsafe { *p.add(ascii.len()) == 0 }
}

/// A path as a NUL-terminated wide string, which is what every `W` entry point
/// wants.
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
