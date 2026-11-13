# Finding Compatibility Patches

Many mods need a small patch to work together, such as "Mod A – Mod B Compatibility". DrukhalManager can search the Workshop for patches between mods you run that you haven't subscribed to.

## Before you start

You need a Steam Web API key in **Main Menu → Preferences → Steam**. Without one, the dialog says so and offers to open Preferences. See [[Getting Started]].

## Opening it

* Click **Find More…** on the **Compatibility Patches** header in the load order, or
* while your load order has no compatibility patches, click **Find Compatibility Patches…** under the categories in the sidebar.

## How it searches

Patches list the mods they patch as their **Required items** on the Workshop. The search asks Steam, for each enabled Workshop mod, which items list it that way. With 150 mods it takes about 40 seconds, and you see the progress.

What it finds is kept for three days, so opening it again is instant. The ↻ button at the top searches again. If you close the dialog partway through, the next search picks up where it stopped.

## What it suggests

An item is suggested when **every** mod it requires is enabled and you don't have it installed. Translations are left out.

Each suggestion shows what it needs ("Needs Giga and ACOT, both enabled"), how many subscribers it has and when it was last updated. Patches with the most subscribers come first.

* **Updated Within** hides items that haven't been updated for longer than you choose, a year by default. Some old mods still work fine, but patches usually need updating with the mods they patch, so this list is meant for recent ones.
* **Show Everything That Needs Only Enabled Mods** also lists items whose names and tags don't say they're patches.
* The 🌐 button opens the item's page in Steam (or your browser), where you subscribe.
* ✕ dismisses a suggestion. **Undo** brings it back, and **Show N Dismissed Items Again** at the bottom brings them all back.

## After subscribing

Press **Reload Mods** (F5, or the refresh button at the top left) once Steam has downloaded the patch. Then **Smart Sort** puts it after the mods it patches.

## What it can't find

* Patches that don't list their mods as Required items on the Workshop.
* Patches whose names don't say they're patches, unless you turn on **Show Everything**.
