// Clicking TUBE cycles through the app's four phosphors — green (default),
// amber, white, blue — by setting data-theme on <html>, exactly like the
// "t" easter egg in the real app. The palettes live in style.css, copied
// verbatim from service/web/style.css. Pure progressive enhancement: the
// page is complete without this file.
(function () {
    "use strict";

    var TUBES = ["green", "amber", "white", "blue"];

    var html = document.documentElement;
    var button = document.getElementById("tube");
    var i = 0;

    if (!button) {
        return;
    }

    button.addEventListener("click", function () {
        i = (i + 1) % TUBES.length;
        if (i === 0) {
            html.removeAttribute("data-theme"); // green is :root, no attribute
        } else {
            html.setAttribute("data-theme", TUBES[i]);
        }
        button.textContent = "TUBE: " + TUBES[i].toUpperCase();
    });
})();
