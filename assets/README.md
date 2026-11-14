# Logo

Fatou's logo is the stepped basin triad: three bounded islands from the Newton
fractal for `z³ - 1`, colored by convergence time in Julia red, green, and
purple.

`logo.png` and `logo.svg` use the light-background palette. `logo-dark.png` and
`logo-dark.svg` use softer highlights for dark backgrounds. All have transparent
backgrounds. PNG and SVG exports are cropped to the mark with a small, even
inset. The VS Code icon centers the cropped mark on a square canvas.

The landing page uses a 1440-pixel-wide `logo-hero.png`, rendered directly from
a denser fractal grid. The header uses a separate small PNG, 128 pixels tall,
sized for high-density screens.

Run `task logo` to regenerate the master assets, large and small documentation
logos, and VS Code extension icon. Favicons and the Apple touch icon are
installed separately from RealFaviconGenerator with `task icons`. To install a
new bundle, pass its URL with
`task icons -- https://realfavicongenerator.net/files/<id>`.

Edit the social card at `docs/src/public/images/og-image.svg` in Inkscape, then
run `task og-image` to export it as PNG. The social card is maintained manually.
