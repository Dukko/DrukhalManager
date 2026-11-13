# Multiplayer Check

Playing together needs the same game version, mods, load order and mod files. When you get desyncs or "checksum mismatch", the Multiplayer Check finds what differs.

## Steps

1. Open the **Launch** dropdown and choose **Multiplayer Check…**.
2. Choose **Save My Report…** and send the file to the other players. Each of them does the same.
3. Choose **Compare With a Report…** and open a report someone sent you. It lists what differs: game version, mods, load order and individual files.

Turn on **Include All Game Files** to also check localisation, interface and graphics files, which finds modified or damaged game files outside the multiplayer checksum. It's slow the first time.

## Linux and Windows players

The report says whether you run the native Linux build or the Windows one. Players report that the two builds' checksums never match, even with the same mods and files. If you play with Windows players, switch to the Windows build: in Steam, open Stellaris's Properties, and under Compatibility force a Proton version.

The comparison also points out files that differ only in line endings, and files the native Linux build may skip because of how their names are capitalised.
