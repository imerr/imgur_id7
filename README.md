imgur_id7
====
Fast tool to scan for valid 7-long imgur ids for the [ArchiveTeam imgur efforts](https://wiki.archiveteam.org/index.php/Imgur) (not affiliated or endorsed)

Uses supplied http proxies to scan many ids in parallel since imgur does have rate limiting.

Generates ids at random since there's too many ids to reasonably scan in order anyways.

# Usage
```
Usage: imgur_id <output> <concurrent> <proxies>
        output: Path to the output file, will be appended to
        concurrent: How many requests to queue per second max. (actual rate will be slightly lower)
        proxies: Proxy list file in the format of 'PROXY_HOST:PROXY_PORT:PROXY_USER:PROXY_PASSWORD' with one entry per line
                 So for example 'proxy.example.com:1234:username:password123'
                 For each entry, one worker will be spawned.
```