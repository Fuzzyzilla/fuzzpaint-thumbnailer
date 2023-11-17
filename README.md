# fuzzpaint-thumbnailer
Quick and smol [XDG-Compliant](https://specifications.freedesktop.org/thumbnail-spec/thumbnail-spec-latest.html) thumbnailer for [`.fzp` images](https://github.com/Fuzzyzilla/fuzzpaint-vk).

[`IThumbnailerProvider`](https://learn.microsoft.com/en-us/windows/win32/api/thumbcache/nn-thumbcache-ithumbnailprovider) support for Windows comes later!

## Installation
Requires installation of the [`application/x.fuzzpaint-doc` media type](https://github.com/Fuzzyzilla/fuzzpaint-vk/blob/6c6b38f050e6be3b91c33c8afa97b1b13abdc8a1/shell/x-fuzzpaint-doc-mime.xml)
from the fuzzpaint project. Download that file, and install with `xdg-mime install x-fuzzpaint-doc-mime.xml`.

After this, install the thumbnailer from this repository:
```bash
cargo build --release
sudo cp target/release/fuzzpaint-thumbnailer /usr/local/bin
sudo chmod +x-w /usr/local/bin/fuzzpaint-thumbnailer
sudo cp fuzzpaint.thumbnailer /usr/share/thumbnailers
```

You may need to restart your shell, file explorer, and/or clear your thumbnail cache (`~/.cache/thumbnails/*`) to see results.
