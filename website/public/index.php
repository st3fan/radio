<?php

declare(strict_types=1);

require_once __DIR__ . '/../lib/daemon.php';
require_once __DIR__ . '/../lib/somafm.php';

// Actions: POST-redirect-GET, so a refresh never repeats an action. Errors
// travel back via a query parameter.
if (($_SERVER['REQUEST_METHOD'] ?? 'GET') === 'POST') {
    $result = handle_action($_POST);
    $query = ($result['ok'] ?? false) ? '' : '?error=' . urlencode($result['error'] ?? 'action failed');
    header('Location: index.php' . $query, true, 303);
    exit;
}

function handle_action(array $post): array
{
    switch ($post['action'] ?? '') {
        case 'play':
            $channels = somafm_channels();
            if ($channels === null) {
                return ['ok' => false, 'error' => 'channel list is unavailable'];
            }
            $url = somafm_playlist_url($channels, (string) ($post['channel'] ?? ''));
            if ($url === null) {
                return ['ok' => false, 'error' => 'unknown channel'];
            }
            return radio_play($url);
        case 'stop':
            return radio_stop();
        case 'pause':
            return radio_pause();
        case 'resume':
            return radio_resume();
        case 'mute':
            return radio_mute();
        case 'unmute':
            return radio_unmute();
        case 'volume':
            $percent = filter_var(
                $post['volume'] ?? '',
                FILTER_VALIDATE_INT,
                ['options' => ['min_range' => 0, 'max_range' => 100]]
            );
            if ($percent === false) {
                return ['ok' => false, 'error' => 'volume must be a number between 0 and 100'];
            }
            return radio_volume($percent);
        default:
            return ['ok' => false, 'error' => 'unknown action'];
    }
}

function h(string $text): string
{
    return htmlspecialchars($text, ENT_QUOTES);
}

$error = isset($_GET['error']) ? (string) $_GET['error'] : null;
$daemon = radio_status();
$status = ($daemon['ok'] ?? false) ? $daemon['status'] : null;
$channels = somafm_channels();

// The percentage the volume form should show for the current effective
// volume (the API takes percent-of-max; the status reports the effective
// device volume).
$volume_percent = 50;
if ($status !== null && (int) $status['max_volume'] > 0) {
    $volume_percent = (int) round((int) $status['volume'] * 100 / (int) $status['max_volume']);
}

?>
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Radio</title>
</head>
<body>
<h1>Radio</h1>

<?php if ($error !== null): ?>
<p><strong>Error:</strong> <?= h($error) ?></p>
<?php endif; ?>

<?php if ($status === null): ?>
<p><strong>Radio daemon:</strong> <?= h($daemon['error'] ?? 'not reachable') ?></p>
<?php else: ?>
<h2>Now</h2>
<p>
State: <strong><?= h((string) $status['state']) ?></strong>
<?php if ($status['muted']): ?>(muted)<?php endif; ?><br>
<?php if (!empty($status['icy_name'])): ?>
Station: <?= h((string) $status['icy_name']) ?><br>
<?php endif; ?>
<?php if (!empty($status['icy_title'])): ?>
Playing: <strong><?= h((string) $status['icy_title']) ?></strong><br>
<?php endif; ?>
Volume: <?= (int) $status['volume'] ?> (max <?= (int) $status['max_volume'] ?>)
</p>

<p>
<?php if ($status['state'] === 'playing'): ?>
<form method="post"><button name="action" value="pause">Pause</button></form>
<?php elseif ($status['state'] === 'paused'): ?>
<form method="post"><button name="action" value="resume">Resume</button></form>
<?php endif; ?>
<?php if ($status['state'] !== 'stopped'): ?>
<form method="post"><button name="action" value="stop">Stop</button></form>
<?php endif; ?>
<?php if ($status['muted']): ?>
<form method="post"><button name="action" value="unmute">Unmute</button></form>
<?php else: ?>
<form method="post"><button name="action" value="mute">Mute</button></form>
<?php endif; ?>
</p>

<form method="post">
<label>Volume (% of max):
<input type="number" name="volume" min="0" max="100" value="<?= $volume_percent ?>">
</label>
<button name="action" value="volume">Set</button>
</form>
<?php endif; ?>

<h2>Channels</h2>
<?php if ($channels === null): ?>
<p>Could not load the SomaFM channel list.</p>
<?php else: ?>
<table border="1" cellpadding="4">
<tr><th></th><th>Channel</th><th>Genre</th><th>Listeners</th><th>Description</th></tr>
<?php foreach ($channels as $channel): ?>
<?php
$id = (string) ($channel['id'] ?? '');
if ($id === '') {
    continue;
}
$is_current = $status !== null
    && ($status['playlist_url'] ?? null) === somafm_playlist_url($channels, $id);
?>
<tr>
<td>
<form method="post">
<input type="hidden" name="channel" value="<?= h($id) ?>">
<button name="action" value="play"><?= $is_current ? '&#9654; Playing' : 'Play' ?></button>
</form>
</td>
<td><?= $is_current ? '<strong>' : '' ?><?= h((string) ($channel['title'] ?? $id)) ?><?= $is_current ? '</strong>' : '' ?></td>
<td><?= h((string) ($channel['genre'] ?? '')) ?></td>
<td><?= (int) ($channel['listeners'] ?? 0) ?></td>
<td><?= h((string) ($channel['description'] ?? '')) ?></td>
</tr>
<?php endforeach; ?>
</table>
<?php endif; ?>

</body>
</html>
