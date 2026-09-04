# OBS Studio capture hook binaries

These files are unmodified binaries from the official OBS Studio 32.2.2 release for Windows x64, taken from `data/obs-plugins/win-capture/` inside `OBS-Studio-32.2.2-Windows-x64.zip` (<https://github.com/obsproject/obs-studio/releases/tag/32.2.2>). They implement the game capture hook: `graphics-hook*.dll` is injected into the game and copies each presented frame into a shared texture, `inject-helper*.exe` performs the injection, and `get-graphics-offsets*.exe` reports the vtable offsets the hook needs. All six are Authenticode signed by "OBS Project, LLC"; anti-cheat vendors whitelist that signature, which is why they are shipped as is and must never be modified or re-signed.

OpenClips only talks to them through their public surface: the inject helper's command line and the capture hook's named shared memory, events and mutexes (the protocol in `shared/obs-hook-config/graphics-hook-info.h` of the OBS source tree). OpenClips does not link against OBS code.

## License

OBS Studio is licensed under the GNU General Public License version 2 or later; the full text is in `COPYING` in this directory. These binaries are redistributed under that license as separate programs aggregated with OpenClips (which remains MIT). The complete corresponding source code is available at <https://github.com/obsproject/obs-studio/tree/32.2.2>; the hook version they implement is 1.8.8 (`shared/obs-hook-config/graphics-hook-ver.h` at that tag).

## Provenance

SHA256 of the vendored files:

```text
566b095dd1de495a3f0233cb08d75ab3e7d7b184c80e2fc23f23f22ca8558632  graphics-hook32.dll
49c0ddeac72b130d4f8ae90510219949022c1f992adb48d81a4972f2cd6c2585  graphics-hook64.dll
4627f12b8295b1ebafd9909dfd0eaa46583e68e8976c8fef76e64122fd2149b6  get-graphics-offsets32.exe
79ed6d2a983cc93a7ad6b96eb9d0fae21871988a2823cce11d4060ace35753fb  get-graphics-offsets64.exe
226da6b414470d17a4b4d368bb55916ab3922b1ae10bb2325ea627f63b1aea71  inject-helper32.exe
a785ae14b5debae83404942d5ef7a51c83a53c603d6f5a23162d7f52ff96d9ff  inject-helper64.exe
```

To update: download the new OBS release zip, replace the six files, verify the signatures (`Get-AuthenticodeSignature`), refresh the hashes above, and re-check that `struct hook_info` in `graphics-hook-info.h` still matches `capture/src/windows/game_capture/protocol.rs` (the struct is versioned and OBS treats it as a frozen ABI, but the check is cheap).
