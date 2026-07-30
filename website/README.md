# Radio website

Plain PHP site that lists the SomaFM channels and controls `radiod` (which
must be running on `127.0.0.1:8080` — see `../service/README.md`). All
daemon calls happen server-side; the browser never talks to the daemon.

No framework, no Composer — needs only PHP with the standard `json`
extension (any Debian `php-cli`/`php-fpm` package qualifies; HTTP uses
stream contexts, so php-curl is not required). `allow_url_fopen` must be
enabled (it is by default).

## Running (development)

```
php -S 0.0.0.0:8000 -t public
```

Then browse to `http://<host>:8000/`. Production serving (lighttpd/nginx +
php-fpm on the Pi) is set up in the deployment milestone.

## Layout

```
public/       docroot — index.php is the whole UI
lib/
  config.php  daemon URL, timeouts, channel cache location/TTL
  daemon.php  client for the radiod REST API
  somafm.php  channels.json fetching, disk cache, playlist selection
```

The channel list is cached on disk (`/tmp/radio-channels.json`, 5-minute
TTL); when SomaFM is unreachable the site serves the stale cache instead.

## Checks

```
php -l public/index.php lib/config.php lib/daemon.php lib/somafm.php
```
