# Working on hydra

## Never stop hydraui by name

You are probably running inside hydraui: the user's installed copy hosts their terminals,
Claude sessions included. Every copy, dev builds too, is called `hydraui.exe`, so
`taskkill /IM hydraui.exe`, `Stop-Process -Name hydraui` or `pkill hydraui` closes the
user's window and every session in it, yours too, with no warning.

To close a dev build you started, stop it by the PID you started, or run
`scripts\stop-dev-hydraui.ps1`, which stops only copies under a `target\` folder.
