# windows-projfs
[![GitHub build status](https://github.com/WolverinDEV/windows-projfs/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/WolverinDEV/windows-projfs/actions?query=workflow%3ARust)
[![crates.io](https://img.shields.io/crates/v/windows-projfs.svg)](https://crates.io/crates/windows-projfs)

A simple rust library for easily creating projected file systems on Windows.  
Examples can be found in [examples](examples).  

## Attention
You must ensure, that the projected file system is enabled on your machine.  
You can enable the projected file system with this command:  
```ps1
Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart
```
More information can be found here:  
https://learn.microsoft.com/en-us/windows/win32/projfs/enabling-windows-projected-file-system