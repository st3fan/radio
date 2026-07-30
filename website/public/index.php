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

/** The terminal volume bar: [██████░░░░░░░░░░░░░░] 25/50 */
function volume_bar(int $volume, int $max_volume): string
{
    $segments = 20;
    $filled = $max_volume > 0 ? (int) round($volume * $segments / $max_volume) : 0;
    $filled = max(0, min($segments, $filled));
    return '[' . str_repeat('█', $filled) . str_repeat('░', $segments - $filled) . "] {$volume}/{$max_volume}";
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

$prompt = 'STANDBY';
if ($status !== null && $status['state'] === 'playing') {
    $prompt = 'NOW PLAYING';
} elseif ($status !== null && $status['state'] === 'paused') {
    $prompt = 'PAUSED';
}

?>
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Radio</title>
<link rel="stylesheet" href="style.css">
<script src="radio.js" defer></script>
</head>
<body>

<header class="term-head">
<span>RADIO</span>
<span class="dim">SOMAFM TUNER</span>
</header>

<?php if ($error !== null): ?>
<p class="error">! ERROR: <?= h($error) ?></p>
<?php endif; ?>

<?php if ($status === null): ?>
<section class="now">
<div class="prompt dim">&gt; STATUS</div>
<p class="error">! DAEMON UNREACHABLE: <?= h($daemon['error'] ?? 'no response') ?></p>
</section>
<?php else: ?>
<?php
// The now-playing block keeps a fixed structure (always a title and a
// station line, possibly empty) so radio.js can update it in place.
$title = (string) ($status['icy_title'] ?? '');
$title_dim = false;
if ($title === '' && $status['state'] === 'stopped') {
    $title = '— NO SIGNAL —';
    $title_dim = true;
}
?>
<section class="now" id="now">
<div class="prompt dim">&gt; <?= h($prompt) ?></div>
<h1 class="title<?= $title_dim ? ' dim' : '' ?>"><?= h($title) ?><?= $status['state'] !== 'stopped' ? '<span class="cursor" aria-hidden="true"></span>' : '' ?></h1>
<div class="station"><?= h((string) ($status['icy_name'] ?? '')) ?></div>
<div class="vol dim">VOL <?= volume_bar((int) $status['volume'], (int) $status['max_volume']) ?><?= $status['muted'] ? ' · MUTED' : '' ?></div>
</section>

<section class="controls">
<?php if ($status['state'] === 'playing'): ?>
<form method="post"><button name="action" value="pause">[PAUSE]</button></form>
<?php elseif ($status['state'] === 'paused'): ?>
<form method="post"><button name="action" value="resume">[RESUME]</button></form>
<?php endif; ?>
<?php if ($status['state'] !== 'stopped'): ?>
<form method="post"><button name="action" value="stop">[STOP]</button></form>
<?php endif; ?>
<?php if ($status['muted']): ?>
<form method="post"><button name="action" value="unmute">[UNMUTE]</button></form>
<?php else: ?>
<form method="post"><button name="action" value="mute">[MUTE]</button></form>
<?php endif; ?>
<form method="post">
<input type="hidden" name="volume" value="<?= max(0, $volume_percent - 10) ?>">
<button name="action" value="volume">[VOL-]</button>
</form>
<form method="post">
<input type="hidden" name="volume" value="<?= min(100, $volume_percent + 10) ?>">
<button name="action" value="volume">[VOL+]</button>
</form>
</section>

<form method="post" class="volform">
<label>VOL%
<input type="number" name="volume" min="0" max="100" value="<?= $volume_percent ?>">
</label>
<button name="action" value="volume">[SET]</button>
</form>
<?php endif; ?>

<section class="channels">
<div class="heading dim">&gt; CHANNELS</div>
<?php if ($channels === null): ?>
<p class="error">! CHANNEL LIST UNAVAILABLE</p>
<?php else: ?>
<table>
<tr><th>CH</th><th>STATION</th><th class="col-genre">GENRE</th><th class="num">LSNRS</th><th></th></tr>
<?php $ch = 0; ?>
<?php foreach ($channels as $channel): ?>
<?php
$id = (string) ($channel['id'] ?? '');
if ($id === '') {
    continue;
}
$ch++;
$is_current = $status !== null
    && ($status['playlist_url'] ?? null) === somafm_playlist_url($channels, $id);
$genre = str_replace('|', ' · ', (string) ($channel['genre'] ?? ''));
?>
<tr<?= $is_current ? ' class="playing"' : '' ?>>
<td><?= str_pad((string) $ch, 2, '0', STR_PAD_LEFT) ?></td>
<td title="<?= h((string) ($channel['description'] ?? '')) ?>"><?= $is_current ? '&gt; ' : '' ?><?= h((string) ($channel['title'] ?? $id)) ?></td>
<td class="col-genre genre"><?= h($genre) ?></td>
<td class="num"><?= (int) ($channel['listeners'] ?? 0) ?></td>
<td>
<?php if ($is_current): ?>
<span class="onair">[ON AIR]</span>
<?php else: ?>
<form method="post">
<input type="hidden" name="channel" value="<?= h($id) ?>">
<button name="action" value="play">[PLAY]</button>
</form>
<?php endif; ?>
</td>
</tr>
<?php endforeach; ?>
</table>
<?php endif; ?>
</section>

<footer class="dim">RADIO · CHANNEL DATA © SOMAFM, CACHED 5 MIN · LISTENER-SUPPORTED RADIO</footer>

</body>
</html>
