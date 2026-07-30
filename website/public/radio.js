// Live now-playing: polls status.php and updates the display in place.
// Progressive enhancement only — without this file the page works exactly
// the same, just via reloads. Controls stay plain forms; this script never
// touches them. The blinking cursor doubles as the "updates are alive"
// indicator: it only blinks (body.live) while polling succeeds.

(function () {
    "use strict";

    var now = document.getElementById("now");
    if (now === null) {
        return; // daemon-unreachable page: nothing to update until reload
    }
    var prompt = now.querySelector(".prompt");
    var title = now.querySelector(".title");
    var station = now.querySelector(".station");
    var vol = now.querySelector(".vol");
    if (!prompt || !title || !station || !vol) {
        return;
    }

    var cursor = title.querySelector(".cursor") || document.createElement("span");
    cursor.className = "cursor";
    cursor.setAttribute("aria-hidden", "true");

    function bar(volume, max) {
        var segments = 20;
        var filled = max > 0 ? Math.round((volume * segments) / max) : 0;
        filled = Math.max(0, Math.min(segments, filled));
        return "[" + "█".repeat(filled) + "░".repeat(segments - filled) + "] " + volume + "/" + max;
    }

    function apply(s) {
        var label = s.state === "playing" ? "NOW PLAYING" : s.state === "paused" ? "PAUSED" : "STANDBY";
        prompt.textContent = "> " + label;

        if (s.icy_title) {
            title.textContent = s.icy_title;
            title.classList.remove("dim");
        } else if (s.state === "stopped") {
            title.textContent = "— NO SIGNAL —";
            title.classList.add("dim");
        } else {
            title.textContent = "";
            title.classList.remove("dim");
        }
        if (s.state !== "stopped") {
            title.appendChild(cursor);
        }

        station.textContent = s.icy_name || "";
        vol.textContent = "VOL " + bar(s.volume, s.max_volume) + (s.muted ? " · MUTED" : "");
    }

    function tick() {
        fetch("status.php")
            .then(function (response) {
                if (!response.ok) {
                    throw new Error("status " + response.status);
                }
                return response.json();
            })
            .then(function (status) {
                document.body.classList.add("live");
                apply(status);
            })
            .catch(function () {
                document.body.classList.remove("live");
            });
    }

    setInterval(tick, 5000);
    tick();
})();
