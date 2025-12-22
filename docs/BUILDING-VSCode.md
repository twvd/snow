# Building the Windows version in VS Code (MSYS2 / MinGW64)

This guide reproduces the GitHub Actions Windows build locally using Visual Studio Code and MSYS2 (MinGW64).

## Prerequisites
- Windows 10/11
- MSYS2 (https://www.msys2.org/)
- Rust (the repo provides `rust-toolchain.toml`, e.g. 1.90.0)
- VS Code

## 1. Install MSYS2 MinGW64

1. Install MSYS2 from https://www.msys2.org/ and open the *MSYS2 MinGW 64-bit* shell.

2. Add new Terminal Profiles for MSYS2 & MSYS2/MingW terminals to your User Settings `settings.json` file. **RECOMMENDED**
    1. Go to the settings by clicking on the gear icon in the lower left corner and selecting Settings.<br>
    2. Open the settings JSON file by searching for `settings.json` in the settings search bar.<br>
    3. Once editing `settings.json` scroll to `terminal.integrated.profiles.windows` and add two new terminals:
          ```json
          {
            // Add these under "terminal.integrated.profiles.windows"
            "terminal.integrated.profiles.windows": {
              "MSYS2": {
                "path": "C:\\msys64\\msys2_shell.cmd",
                "args": ["-defterm", "-here", "-no-start"]
              },
              "MSYS2 MinGW64": {
                "path": "C:\\msys64\\usr\\bin\\bash.exe",
                "args": ["-lc", "env MSYSTEM=MINGW64 /usr/bin/bash -l -i"]
              }
            }
          }
          ```

3. Update packages and install required tooling (run in the MINGW64 shell):
    ```sh
    pacman -Syu
    # close/reopen MSYS2 MinGW64 shell
    pacman -Su
    pacman -S --noconfirm \
      mingw-w64-x86_64-toolchain \
      mingw-w64-x86_64-pkg-config \
      mingw-w64-x86_64-SDL2 \
      mingw-w64-x86_64-cmake
    ```

    Notes:
    - `mingw-w64-x86_64-toolchain` provides gcc, windres and binutils to match CI.
    - Using the MinGW64 shell (or ensuring `C:\msys64\mingw64\bin` is on PATH) avoids mixing MSVC and MinGW runtimes.

## 2. Install Rust toolchain into MinGW64 environment

The repository includes `rust-toolchain.toml` (toolchain 1.90.0)

Use MSYS2's Rust packages (inside the MinGW64 shell):
```sh
pacman -S --noconfirm mingw-w64-x86_64-rust mingw-w64-x86_64-rust-src
```

## 3. Verify required tools

Open the MSYS2 MinGW64 terminal in VS Code and verify:
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
If `pkg-config` or `sdl2` fail, ensure you're running in the MinGW64 shell or that `C:\msys64\mingw64\bin` is on PATH.

## 4. Configure project to use MSYS2 MinGW64 terminal

Create or update Workspace Settings in `/.vscode/settings.json` to prefer the MSYS2 MinGW64 terminal:

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

## 5. Add the build task

Create `/.vscode/tasks.json` to add a build task:

```json
// filepath: ..\snow\.vscode\tasks.json
{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "build",
      "type": "shell",
      "command": "cargo",
      "args": ["build", "--verbose"],
      "group": { "kind": "build", "isDefault": true },
      "problemMatcher": ["$rustc"]
    }
  ]
}
```

## 6. Install Rust-Analyzer (optional)
Install "Rust Analyzer" via extension manager

> NOTE: This extension tries to detect and auto-select the best rust environment for projects automatically. If you have multiple rust locations, or multiple toolchains installed, you may have to override its settings to point and use the correct environment and toolchain. If you get ABI compatibility errors or statement errors about missing std library from the extension, check this setting in your Workspace Settings

```json
// filepath: ..\snow\.vscode\settings.json

// Change this to your mingw folder if it's not discovering automatically
//"rust-analyzer.cargo.sysroot": "discover", 
"rust-analyzer.cargo.sysroot": "C:\\msys64\\mingw64",

// Your local mingw bin folder, your profile cargo bin need to be included if not detected correctly:
"rust-analyzer.server.extraEnv": {"PATH":"C:\\msys64\\mingw64\\bin;%USERPROFILE%\\.cargo\\bin;${env:PATH}"}

// If the extension tries to comile and check every target and you are missing toolchains, or simply do not wish to check all targets on change, set allTargets to 'false'
"rust-analyzer.cargo.allTargets": false,

```

## 7. Verify Workspace Config
If you've followed this guide on the clean VS Code installation and used default paths when installing the components listed above, your Workspace Settings `settings.json` should closely resemble the file below. 

```json
// filepath: ..\snow\.vscode\settings.json

{
  // Entry to add MSYS2/MingW64 Terminal
  "terminal.integrated.profiles.windows": {
    "MSYS2 MinGW64": {
      "path": "C:\\msys64\\usr\\bin\\bash.exe",
      "args": ["-lc", "env MSYSTEM=MINGW64 /usr/bin/bash -l -i"],
      "env": { 
        "PATH": "C:\\msys64\\usr\\bin;%USERPROFILE%\\.cargo\\bin;${env:PATH}"
      }
    }
  },

  // Tell VS Code to use this as the default Terminal for this project:
  "terminal.integrated.defaultProfile.windows": "MSYS2 MinGW64",

  // Optional - Add these to integrate rust-analyzer extension to MingW64/GNU toolchain
  "rust-analyzer.cargo.sysroot": "discover",

  // If you need to force PATH to pick up MinGW tools:
  "rust-analyzer.server.extraEnv": {
    "PATH": "C:\\msys64\\mingw64\\bin;%USERPROFILE%\\.cargo\\bin;${env:PATH}"
  },

  // Avoid checking every target on each change:
  "rust-analyzer.cargo.allTargets": false
}
```

## 8. Verify Manual compilation is working

From the MSYS2 MinGW64 terminal in VS Code, navigate to the project folder and run:
```sh
cargo build --release --manifest-path frontend_egui/Cargo.toml
```

## Troubleshooting
If you are usung rustup managed toolchains and are running into an issue where extensions or tasks are attempting to use MSVC toolchain (wrong ABI for this), doublecheck your default toolchain, you may have to force it to use GNU. From the MSYS2 MingW64 Terminal, run:
```sh
# See toolchains in your environment:
rustup show

# Change your 'default' to GNU:
rustup default 1.90.0-x86_64-pc-windows-gnu
```

