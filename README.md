imgur_id7
====
Fast tool to scan for valid 7-long imgur ids for the [ArchiveTeam imgur efforts](https://wiki.archiveteam.org/index.php/Imgur) (not affiliated or endorsed)

Uses supplied http proxies to scan many ids in parallel since imgur does have rate limiting.

Generates ids at random since there's too many ids to reasonably scan in order anyways.

# Usage
```
Usage: imgur_id <output> <concurrent> [<proxies=--no-proxies>]
	output: Path to the output file, will be appended to
	concurrent: How many requests to queue per second max. (actual rate will be slightly lower)
	proxies: Proxy list file or --no-proxies (default) to not use proxies
	         Proxy list file has the format of 'PROXY_HOST:PROXY_PORT:PROXY_USER:PROXY_PASSWORD' with one entry per line
	         So for example 'proxy.example.com:1234:username:password123'
	         For each entry, one worker will be spawned.
```

# Building
Github Actions are set up to provide builds, but especially the linux ones might not run on your distro

Building is easy though!

1. [Install rust](https://www.rust-lang.org/tools/install)
2. Install your platforms compiler toolchain (for debian-based distros this would be `apt install build-essential`, for windows this might be MSVC)
3. Clone this repo or [download it as a .zip](https://github.com/imerr/imgur_id7/archive/refs/heads/main.zip)
4. Run `cargo build --release`* and grab the resulting binary from `target/release/imgur_id7`
5. Success!

*You might have to install library headers like `libssl-dev` and `pkg-config`, but the build process will complain accordingly 
