Capture what is currently on this machine's screen and, where the running
provider can carry a picture, return it so you can look at it.

macOS and Windows. On macOS it shells out to `/usr/sbin/screencapture` and
bounds the result with `/usr/bin/sips`. On Windows it runs one Windows
PowerShell script that uses `System.Drawing`'s `CopyFromScreen` and makes the
bounded copy in the same run. On any other platform the call is refused and the
refusal names the platform.

What you get back:

- One line naming the display or region captured, the pixel bound applied, and
  the format sent.
- The full-resolution PNG's path on disk. It stays there at full size; only the
  copy sent to you is bounded.
- The picture itself, when the provider in use can carry one. When it cannot,
  the result says so and names the reason instead of silently returning prose.
  A provider accepting an image is not the same as the model reading it: a
  text-only model is sent the picture and will not see it, and nothing here can
  detect that.

Arguments:

- `display` (integer, default 1): which display to capture, 1 for the main one.
- `region` (object `{x, y, w, h}`, optional): capture this rectangle instead of
  a whole display, in screen coordinates.
- `cursor` (boolean, default false): draw the mouse pointer into the capture.
  macOS only. On Windows the pointer is not drawn and the result says so, once,
  in the line after the file path.
- `max_dimension` (integer, default 1280, maximum 2000): the longest edge of the
  copy sent to you. Larger costs tokens and gains detail; smaller is cheaper and
  blurs small text.

Window capture is not offered on either platform. `screencapture` selects a
window by a numeric id that nothing here can enumerate, so the only honest
window option would be the interactive picker, which waits for a human click.

What can go wrong is different on the two platforms, and only one of the two
failures is undetectable.

On macOS, Screen Recording permission is required and is granted per
application. Without it the capture is refused or comes back as the desktop
wallpaper alone; the refusal is detected and reported, the wallpaper case cannot
be, so a screenshot that shows no windows probably means the permission is
missing. Grant it in System Settings > Privacy & Security > Screen Recording.

On Windows there is no per-application permission and no wallpaper case, so a
capture that succeeds is the desktop. What fails instead is a process with no
interactive desktop to read — a service, a scheduled task set to run whether the
user is logged on or not, or a locked session — and that shows up as a black
image at the right size rather than as an error. Two further Windows notes: the
capture is composited at the desktop's virtualised resolution, so on a display
with a scaling factor the pixel dimensions are smaller than the physical panel;
and `display: 1` is the primary display, with the rest in the order Windows
enumerates them.

Files land in one directory per session under the system temporary directory.
Nothing here deletes them; they live until the operating system clears its
temporary directory, which macOS does on reboot and after three days of disuse,
and which Windows does not do on its own at all.
