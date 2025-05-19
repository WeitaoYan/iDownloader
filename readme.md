# 分片下载器

Rust 实现，异步分片下载，如果 server 支持的话。

```
Usage: iDownloader [OPTIONS] <URL>

Arguments:
  <URL>  URL to download

Options:
  -o, --output <DIR>       Output directory
  -m, --max-chunks <NUM>   Maximum number of chunks [default: 500]
  -r, --max-retries <NUM>  Maximum number of retries [default: 3]
  -h, --help               Print help
  -V, --version            Print version
```
