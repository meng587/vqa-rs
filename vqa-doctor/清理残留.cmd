@echo off
cd /d "%~dp0"
"%~dp0target\release\vqa-doctor.exe" --clean
echo.
pause