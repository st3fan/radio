<?php

declare(strict_types=1);

// JSON proxy of the daemon's GET /status for the page's live updates. The
// browser still never talks to the daemon directly.

require_once __DIR__ . '/../lib/daemon.php';

header('Content-Type: application/json');
header('Cache-Control: no-store');

$result = radio_status();
if (!($result['ok'] ?? false)) {
    http_response_code(502);
    echo json_encode(['error' => $result['error'] ?? 'daemon unreachable']);
    exit;
}
echo json_encode($result['status']);
