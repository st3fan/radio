<?php

declare(strict_types=1);

require_once __DIR__ . '/config.php';

/**
 * Performs one HTTP request against the daemon.
 *
 * Returns ['ok' => true, 'status' => <decoded status JSON>] on success, or
 * ['ok' => false, 'error' => <message>] when the daemon is unreachable,
 * answers garbage, or returns a 4xx/5xx (whose JSON error message is
 * surfaced, e.g. the 409 from pausing while stopped).
 */
function radio_request(string $method, string $path, ?array $body = null): array
{
    $http = [
        'method' => $method,
        'timeout' => RADIO_HTTP_TIMEOUT,
        'ignore_errors' => true, // fetch the body of 4xx/5xx responses too
    ];
    if ($body !== null) {
        $http['header'] = 'Content-Type: application/json';
        $http['content'] = json_encode($body);
    }
    $context = stream_context_create(['http' => $http]);

    $raw = @file_get_contents(RADIO_DAEMON_URL . $path, false, $context);
    if ($raw === false) {
        return ['ok' => false, 'error' => 'the radio daemon is not reachable'];
    }

    $code = 0;
    foreach ($http_response_header ?? [] as $line) {
        if (preg_match('#^HTTP/\S+\s+(\d{3})#', $line, $m)) {
            $code = (int) $m[1];
        }
    }

    $json = json_decode($raw, true);
    if (!is_array($json)) {
        return ['ok' => false, 'error' => 'unexpected response from the radio daemon'];
    }
    if ($code >= 400) {
        $message = is_string($json['error'] ?? null) ? $json['error'] : "daemon returned HTTP $code";
        return ['ok' => false, 'error' => $message];
    }
    return ['ok' => true, 'status' => $json];
}

function radio_status(): array
{
    return radio_request('GET', '/status');
}

function radio_play(string $playlist_url): array
{
    return radio_request('POST', '/play', ['playlist_url' => $playlist_url]);
}

function radio_stop(): array
{
    return radio_request('POST', '/stop');
}

function radio_pause(): array
{
    return radio_request('POST', '/pause');
}

function radio_resume(): array
{
    return radio_request('POST', '/resume');
}

/** $percent is 0-100, a percentage of the daemon's max_volume. */
function radio_volume(int $percent): array
{
    return radio_request('POST', '/volume', ['volume' => $percent]);
}

function radio_mute(): array
{
    return radio_request('POST', '/mute');
}

function radio_unmute(): array
{
    return radio_request('POST', '/unmute');
}
