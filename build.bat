cd %~dp0
cargo build --release --target i686-pc-windows-gnu && cargo build --release --target x86_64-pc-windows-gnu && %WIXSHARP_DIR%\cscs.exe wixcs\setup.cs
