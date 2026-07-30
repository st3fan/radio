<?php

declare(strict_types=1);

// The radiod REST API. Always loopback: the browser never talks to the
// daemon, this site does, server-side.
const RADIO_DAEMON_URL = 'http://127.0.0.1:8080';

// Seconds before we give up on the daemon (it answers in milliseconds when
// it is up; this only matters when it is down).
const RADIO_HTTP_TIMEOUT = 1.0;

const SOMAFM_CHANNELS_URL = 'https://api.somafm.com/channels.json';
const SOMAFM_FETCH_TIMEOUT = 5.0;
const SOMAFM_CACHE_TTL = 300; // seconds

function somafm_cache_path(): string
{
    return sys_get_temp_dir() . '/radio-channels.json';
}
