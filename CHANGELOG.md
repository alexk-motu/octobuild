# Changes

## 0.1.15:

 * Clang: Feature "remove comments from clang preprocessed output" is removed
   from Octobuild as unsafe (fix #23).

   To increase cache hit on Unreal Engine build please patch UE4
   like https://github.com/EpicGames/UnrealEngine/pull/3014

## 0.1.14:

 * Linux: Add -nostdinc++ to whitelist (fix #17)
 * VC: Case insensitive precompiled header name lookup

## 0.1.13

 * Linux: Change default cache location from ~/.cache/.octobuild to ~/.cache/octobuild.
 * xgConsole: Remove redundant C4628 warning from output (fix #12).
 * xgConsole: Add file arguments support (fix #13).

## 0.1.12

 * Change configuration format to YAML.

## 0.1.11

 * xgConsole: Add expand path masks on Windows.
 * xgConsole: Allow multiple task files.

## 0.1.10

 * Add configuration file support.

## 0.1.9

 * xgConsole: Add support for tasks file starting from slash.

## 0.1.8

 * VC: Fix error reporting on preprocessor errors.

## 0.1.7

 * xgConsole: Show result already runnging task after first failure.
 * VC: Reduce disk IO.

## 0.1.6

 * Preallocate extracted from cache file for reducing disk fragmentation.

## 0.1.5

 * Add show some cache statistics after build finish.
 * Fix partically saved files from cache on IO-errors (like out-disk-space).
 * Clang: Don't use octobuild on --analyze.
 * Clang: Add support cache for cross-compiler.

## 0.1.4

 * Join i686 and x86_64 builds to single .nupkg Chocolatey package (fix #4).
 * Don't require reboot for apply PATH environment variable (fix #9).

## 0.1.3

 * Fix panicked at 'called `Result::unwrap()` on an `Err` value: "SendError(..)"' (fix #8).
 * Minor performance improvement.

## 0.1.2

 * Remove comments from clang preprocessed output for more cache hits.

## 0.1.1

 * Rewrite .deb packaging.

## 0.1.0

 * First release.
