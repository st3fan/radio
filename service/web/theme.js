/* The phosphor: shared by every page of the site.
 *
 * Load it synchronously from <head> (no defer) — the restore below has
 * to run before the first paint, or the tube flashes green on its way
 * to amber. Everything is global-free and idempotent, so a page only
 * has to include the one <script> tag.
 */
(function () {
    var THEMES = ["green", "amber", "white", "blue"];
    var KEY = "radio-theme";
    var DEFAULT = THEMES[0];

    /* "green" is the stylesheet's bare :root, so it carries no
       data-theme attribute — that also keeps the HTMX polling swaps
       (which never touch <html>) from disturbing the theme. */
    function apply(theme) {
        if (theme === DEFAULT) { delete document.documentElement.dataset.theme; }
        else { document.documentElement.dataset.theme = theme; }
    }

    function stored() {
        var theme = null;
        try { theme = localStorage.getItem(KEY); } catch (e) {}
        return THEMES.indexOf(theme) === -1 ? DEFAULT : theme;
    }

    apply(stored());

    /* Easter egg: "t" cycles the tube through the phosphors a CRT
       actually came in. Sticky via localStorage, so the choice follows
       the reader from page to page. */
    document.addEventListener("keydown", function (e) {
        if (e.key !== "t" || e.metaKey || e.ctrlKey || e.altKey) { return; }
        var tag = (e.target.tagName || "").toLowerCase();
        if (tag === "input" || tag === "textarea") { return; }
        var next = THEMES[(THEMES.indexOf(stored()) + 1) % THEMES.length];
        apply(next);
        try { localStorage.setItem(KEY, next); } catch (e2) {}
    });
})();
