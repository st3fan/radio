<?php

declare(strict_types=1);

require_once __DIR__ . '/config.php';

/**
 * Returns the SomaFM channel list sorted by listeners (descending), or
 * null when neither SomaFM nor a cached copy is available.
 */
function somafm_channels(): ?array
{
    $raw = somafm_channels_json();
    if ($raw === null) {
        return null;
    }
    $data = json_decode($raw, true);
    $channels = is_array($data) ? ($data['channels'] ?? null) : null;
    if (!is_array($channels)) {
        return null;
    }
    usort(
        $channels,
        fn (array $a, array $b): int => (int) ($b['listeners'] ?? 0) <=> (int) ($a['listeners'] ?? 0)
    );
    return $channels;
}

/**
 * The raw channels.json: disk-cached for SOMAFM_CACHE_TTL seconds; a failed
 * fetch falls back to a stale cache rather than an empty page.
 */
function somafm_channels_json(): ?string
{
    $cache = somafm_cache_path();
    $mtime = @filemtime($cache);
    if ($mtime !== false && time() - $mtime < SOMAFM_CACHE_TTL) {
        $cached = @file_get_contents($cache);
        if ($cached !== false) {
            return $cached;
        }
    }

    $context = stream_context_create(['http' => ['timeout' => SOMAFM_FETCH_TIMEOUT]]);
    $fresh = @file_get_contents(SOMAFM_CHANNELS_URL, false, $context);
    if ($fresh !== false && json_decode($fresh, true) !== null) {
        @file_put_contents($cache, $fresh, LOCK_EX);
        return $fresh;
    }

    error_log('radio website: fetching channels.json failed, falling back to stale cache');
    $stale = @file_get_contents($cache);
    return $stale === false ? null : $stale;
}

/**
 * The playlist URL to hand to the daemon for a channel id: the highest
 * quality mp3 playlist, then any mp3 playlist, then the conventional
 * somafm.com/{id}.pls. Null for an unknown id — Play forms submit ids, and
 * only URLs from our own channel data ever reach the daemon.
 */
function somafm_playlist_url(array $channels, string $id): ?string
{
    foreach ($channels as $channel) {
        if (($channel['id'] ?? null) !== $id) {
            continue;
        }
        $mp3 = array_values(array_filter(
            is_array($channel['playlists'] ?? null) ? $channel['playlists'] : [],
            fn ($p): bool => is_array($p) && ($p['format'] ?? '') === 'mp3' && is_string($p['url'] ?? null)
        ));
        foreach ($mp3 as $playlist) {
            if (($playlist['quality'] ?? '') === 'highest') {
                return $playlist['url'];
            }
        }
        if ($mp3 !== []) {
            return $mp3[0]['url'];
        }
        return "https://somafm.com/{$id}.pls";
    }
    return null;
}
