# Building the Windows version in VS Code (MSYS2 / MINGW64)

This guide reproduces the GitHub Actions Windows build locally using Visual Studio Code and MSYS2 (MINGW64).

## Prerequisites
- Windows 10/11
- MSYS2 (https://www.msys2.org/)
- Rust (the repo uses the toolchain in `rust-toolchain.toml`, e.g. 1.90.0)
- VS Code

## 1. Install and prepare MSYS2
1. Install MSYS2 from https://www.msys2.org/ and open the *MSYS2 MinGW 64-bit* shell.

Add MSYS2 terminal to VS Code
Configure a terminal profile in your settings.json file. 
-- Go to the settings by clicking on the gear icon in the lower left corner and selecting Settings.
-- Open the settings JSON file by searching for "settings.json" in the settings search bar.
-- Add the following configuration to "terminal.integrated.profiles.windows":
```json
{
  "terminal.integrated.profiles.windows": {
    "MSYS2": {
      "path": "C:\\msys64\\msys2_shell.cmd",
      "args": ["-defterm", "-here", "-no-start"]
    }
  },
  "terminal.integrated.profiles.windows": {
    "MSYS2 MinGW64": {
      "path": "C:\\msys64\\usr\\bin\\bash.exe",
      "args": ["-lc", "env MSYSTEM=MINGW64 /usr/bin/bash -l -i"]
    }
  },
}
```

2. Update packages and install required tooling (run in the MINGW64 shell):
```sh
pacman -Syu                # may require closing & reopening the shell
pacman -Su
pacman -S --noconfirm \
  mingw-w64-x86_64-toolchain \
  mingw-w64-x86_64-pkg-config \
  mingw-w64-x86_64-SDL2 \
  mingw-w64-x86_64-cmake \
```
Notes:
- `mingw-w64-x86_64-toolchain` includes gcc, windres, binutils.
- Use these packages to match the CI MINGW64 environment. 


## 2. Rust toolchain
The repository provides `rust-toolchain.toml`. Make sure you have the matching toolchain:
```sh
# Make sure MSYS's rust package is installed
pacman -S --noconfirm mingw-w64-x86_64-rust \
  mingw-w64-x86_64-rust-src
```
If you installed Rust via MSYS2, you can use that toolchain inside the MINGW64 shell.

## 3. Configure VS Code terminal to use MSYS2 MINGW64
Add or update `.vscode/settings.json` so the integrated terminal opens the MSYS2 shell by default:
```json
// filepath: ..\snow\.vscode\settings.json
{
  
  "terminal.integrated.profiles.windows": {
    "MSYS2 MinGW64": {
      "path": "C:\\msys64\\usr\\bin\\bash.exe",
      "args": ["-lc", "env MSYSTEM=MINGW64 /usr/bin/bash -l -i"]
    }
  },
  "terminal.integrated.defaultProfile.windows": "MSYS2 MinGW64"
  
}
```

## 4. Add a VS Code build task
Create or update `.vscode/tasks.json` to add a build task that runs the same `cargo build` as the GitHub Actions workflow:
```json
{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "build",
      "type": "shell",
      "command": "cargo",
      "args": [
        "build",
        "--verbose"
      ],
      "group": {
        "kind": "build",
        "isDefault": true
      },
      "problemMatcher": ["$rustc"]
    }
  ]
}
```

## 5. Verify required tools
Open the MSYS2 MinGW64 terminal in VS Code and verify that the required tools are available:
```sh
gcc --version
g++ --version
rustc --version
cargo --version
pkg-config --version
pkg-config --libs --cflags sdl2
sdl2-config --version
cmake --version
```
## 6. OPTIONAL - Install Rust-Analyzer
Install via extension manager. If you get complaints about std lib being missing, 
try checking your path variable in settings.json.
```json
  // Change this to your mingw folder if it's not disocvering automatically
  "rust-analyzer.cargo.sysroot": "discover", 
  // Your local mingw bin folder, your profile cargo bin need to be included and may not detect correctly:
  "rust-analyzer.server.extraEnv": {"PATH":"C:\\msys64\\mingw64\\bin;%USERPROFILE%\\.cargo\\bin;${env:PATH}"}
```

## 7. Run manual compile command and verify
Open the MSYS2 MinGW64 terminal in VS Code and verify that compilation works, navigate to the project folder and run:
```sh
cargo build --release --manifest-path frontend_egui/Cargo.toml
```

That's it — open the repo in VS Code, use the MSYS2 MinGW64 terminal (configured above), run the task or cargo command and you should get the Windows binary matching the workflow.