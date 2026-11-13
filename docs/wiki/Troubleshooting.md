# Troubleshooting

## The game logged errors

When the last game wrote errors to its `error.log`, the bottom of the Load Order sidebar says **Last game logged N errors**. Click it to see them grouped by the mod they came from, with your merges and fixes at the top.

## A notification went away before I read it

The bell at the top right keeps every notification, newest first. A badge counts the ones you haven't seen.

## Reporting a bug

1. **Main Menu → Troubleshooting → Copy Diagnostics** copies a short report.
2. Paste it into a [new issue](https://github.com/Dukko/DrukhalManager/issues/new), with what you did and what happened.

The report comes from a small log of scans, merges, AI calls and game launches. It never includes API keys, prompts or file contents. **Open Log Folder** shows the log itself.

## Steam features don't work

Required items and [[Finding Compatibility Patches]] need a Steam Web API key in **Preferences → Steam**. If Steam rejects the key, the patch search says "Steam didn't accept the Steam Web API key": check it at [steamcommunity.com/dev/apikey](https://steamcommunity.com/dev/apikey).

## A mod I subscribed to doesn't show up

Wait for Steam to finish downloading it, then press **Reload Mods** (F5, or the refresh button at the top left).

## Where's the option that used to be in the main menu?

See [[Where Things Are]].
