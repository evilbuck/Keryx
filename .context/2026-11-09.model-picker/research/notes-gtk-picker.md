---
status: completed
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [gtk, fuzzy-search, accessibility, native-ui]
---
# Native GTK picker evidence

## Decision
Reuse the current native `gio::ListStore<glib::BoxedAnyObject>`, `SingleSelection` and `SignalListItemFactory` patterns. Compose a `MenuButton` owning a custom `Popover`, with `SearchEntry` first and a bounded `ScrolledWindow`/`ListView` below it. No browser runtime, new dependency or GTK minimum bump.

## Verified documentation findings
- GTK StringFilter supports Exact, Substring and Prefix, not fuzzy relevance. StringSorter performs collation, not match scoring. A deterministic local subsequence matcher is required for the requested fuzzy filtering/ranking at GTK 4.16.
- CustomFilter/CustomSorter can consume existing structured row objects; StringFilter requires a string-valued expression and is not a direct field reader for BoxedAnyObject. Cache search keys and query scores rather than rescoring within sorter comparisons.
- MenuButton custom child is available since GTK 4.6; can-shrink since 4.12. ListView scroll_to is available since 4.12. SearchEntry search-delay is available since 4.8. These fit the existing GTK 4.16 minimum.
- Popover autohide defaults to true, takes focus, wraps focus and supports Escape dismissal. Put the search entry first, explicitly restore the composer focus/caret, and verify the actual native behavior rather than assuming composition handles all keys.
- SearchEntry activation/stop-search signals support Enter/Escape routing. Use a scoped key controller where Up/Down must move results rather than the search caret; prevent picker Enter from reaching the composer submit handler.
- ListView activation is deliberate. Keep single-click-activate false as the app already does, and commit on explicit row activation rather than selection/hover notification.
- A custom Popover is not automatically a complete accessible menu/combobox. Keep meaningful accessible labels/selection semantics on the trigger, search and native list, and test keyboard-only interaction.

## Search contract
Case-insensitive, separator-normalized, all-token ordered-subsequence matching; exact/prefix/contiguous matches outrank gapped matches, stable ordering resolves ties, empty query returns source order. Search provider display name/slug/server aliases plus model ID. Do not hardcode upstream model aliases. Edit-distance typo correction is out of scope.

[Hermes' upstream fuzzy scorer](https://github.com/NousResearch/hermes-agent/blob/main/apps/shared/src/fuzzy.ts) is a behavior reference, not a runtime dependency or automatically copied source. Numeric weights are not an acceptance contract. Cache normalization once per catalog row and score each row once per query.

## Primary sources
- [StringFilter](https://docs.gtk.org/gtk4/class.StringFilter.html), [match modes](https://docs.gtk.org/gtk4/enum.StringFilterMatchMode.html), [StringSorter](https://docs.gtk.org/gtk4/class.StringSorter.html)
- [gtk-rs 0.10.1 StringFilterMatchMode](https://docs.rs/gtk4/0.10.1/gtk4/enum.StringFilterMatchMode.html)
- [MenuButton](https://docs.gtk.org/gtk4/class.MenuButton.html), [Popover](https://docs.gtk.org/gtk4/class.Popover.html)
- [SearchEntry](https://docs.gtk.org/gtk4/class.SearchEntry.html), [activate](https://docs.gtk.org/gtk4/signal.SearchEntry.activate.html), [stop-search](https://docs.gtk.org/gtk4/signal.SearchEntry.stop-search.html)
- [ListView](https://docs.gtk.org/gtk4/class.ListView.html), [activation](https://docs.gtk.org/gtk4/signal.ListView.activate.html), [single-click-activate](https://docs.gtk.org/gtk4/property.ListView.single-click-activate.html)
- [gtk-rs scroll_to](https://docs.rs/gtk4/0.10.1/gtk4/struct.ListView.html#method.scroll_to), [event-controller propagation](https://docs.rs/gtk4/0.10.1/gtk4/prelude/trait.EventControllerExt.html)

Source types: official documentation and primary source repository. Access date: 2026-11-09. Research handoff: `GtkFuzzyPicker`. No native picker was implemented or exercised in this planning session. That agent's unauthenticated remote limitations are superseded by the parent's authenticated catalog/guard probes.
