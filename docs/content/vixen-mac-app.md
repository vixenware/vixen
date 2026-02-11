+++
title = "Vixen Mac app"
insert_anchor_links = "heading"
+++

Why a mac app? Because we need a VFS, which you can only get through:

  * Networked file systems (meh)
  * MacFUSE Kernel extension (requires rebooting in safe mode and lowering the security of your mac)
  * FSKit (the new shiny thing)
  
And, well, you need a (signed, notarized) app to register an FSKit Filesystem
Extension, and it also serves as a nice onboarding to remind you to _actually turn on_
the extension:

![The vixen mac app showing settings](/static/vixen-app.png)

You can build `Vixen.app` and friends yourself (`just apps/mac/install`) but you won't
be able to do anything useful with it, because it won't be signed by the right team,
and changing the bundle ID won't work either.

Use the official build from the cask:

```shell
$ brew install --cask bearcove/tap/vixen
```

Which is hosted at <https://github.com/bearcove/vixen-mac-app>.
