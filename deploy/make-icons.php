<?php

declare(strict_types=1);

// Dev-time icon generator (needs php-gd): draws the pixel-art phosphor
// icon — a blocky R with a cursor block — from the ASCII grid below and
// writes the PNGs into website/public/. The PNGs are committed, so
// package builds never need GD; re-run this only to change the icon.
//
//   php deploy/make-icons.php

const GROUND = [0x05, 0x08, 0x05];
const PHOSPHOR = [0x35, 0xe0, 0x6b];

// 16x16 grid; '#' is a lit cell. A chunky R and the block cursor.
const GRID = [
    '................',
    '................',
    '..#######.......',
    '..########......',
    '..##....##......',
    '..##....##......',
    '..##....##......',
    '..########......',
    '..#######.......',
    '..##..##........',
    '..##...##.......',
    '..##....##......',
    '..##.....##.##..',
    '..##......#.##..',
    '................',
    '................',
];

function render(int $size, float $content_scale, string $path): void
{
    $image = imagecreatetruecolor($size, $size);
    $ground = imagecolorallocate($image, ...GROUND);
    $phosphor = imagecolorallocate($image, ...PHOSPHOR);
    imagefilledrectangle($image, 0, 0, $size, $size, $ground);

    $cells = count(GRID);
    $content = (int) round($size * $content_scale);
    $cell = $content / $cells;
    $origin = ($size - $content) / 2;

    foreach (GRID as $y => $row) {
        for ($x = 0; $x < strlen($row); $x++) {
            if ($row[$x] !== '#') {
                continue;
            }
            imagefilledrectangle(
                $image,
                (int) round($origin + $x * $cell),
                (int) round($origin + $y * $cell),
                (int) round($origin + ($x + 1) * $cell) - 1,
                (int) round($origin + ($y + 1) * $cell) - 1,
                $phosphor
            );
        }
    }

    imagepng($image, $path);
    echo "$path ({$size}x{$size})\n";
}

$out = __DIR__ . '/../website/public';
render(180, 1.0, "$out/icon-180.png");
render(192, 1.0, "$out/icon-192.png");
render(512, 1.0, "$out/icon-512.png");
// Maskable: content shrunk into the safe zone so circular masks don't
// clip the glyph.
render(512, 0.62, "$out/icon-maskable-512.png");
