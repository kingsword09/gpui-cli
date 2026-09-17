@echo off
echo === Installing GPUI CLI ===

where cargo >nul 2>nul
if %ERRORLEVEL% neq 0 (
    echo Error: cargo is not installed. Please install Rust from https://rustup.rs/
    exit /b 1
)

echo Building and installing gpui binary...
cargo install --path "%~dp0." --force --locked
if errorlevel 1 exit /b 1

echo.
echo Installation successful!
echo Run 'gpui --help' or 'gpui init' to get started.
