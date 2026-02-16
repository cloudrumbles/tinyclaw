---
name: "miniapps"
description: "Create Telegram Mini Apps — interactive web UIs that open inside Telegram's built-in browser. Use when the user asks for an interactive interface, dashboard, tracker, form, quiz, game, or any rich UI that goes beyond plain text messages."
---

# Telegram Mini Apps

Build interactive web applications that run inside Telegram's embedded browser. Mini apps are HTML/CSS/JS pages served by tinyclaw and opened via an inline keyboard button in the chat.

## When to use

- User asks for an interactive UI (tracker, dashboard, form, quiz, calculator, game)
- The response needs rich formatting, charts, or interactivity beyond what text messages can provide
- User wants a persistent tool they can reopen (e.g. nutrition tracker, habit tracker, todo list)

## How it works

1. You create an HTML file (with inline CSS/JS) at `~/.tinyclaw/miniapps/{app-name}/index.html`
2. Include `[miniapp: {app-name}: {Button Text}]` in your response text
3. Tinyclaw strips the tag, serves the file, and sends an inline button to the user
4. User taps the button — the mini app opens inside Telegram

## File structure

```
~/.tinyclaw/miniapps/
  nutrition-tracker/
    index.html      <- entry point (required)
    (additional .js, .css, images if needed)
```

Keep it as a single `index.html` with inline CSS and JS when possible. This avoids issues with relative paths and makes the app self-contained.

## Template

```html
<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no">
  <title>App Name</title>
  <script src="https://telegram.org/js/telegram-web-app.js"></script>
  <style>
    :root {
      --tg-theme-bg-color: var(--tg-theme-bg-color, #ffffff);
      --tg-theme-text-color: var(--tg-theme-text-color, #000000);
      --tg-theme-hint-color: var(--tg-theme-hint-color, #999999);
      --tg-theme-link-color: var(--tg-theme-link-color, #2481cc);
      --tg-theme-button-color: var(--tg-theme-button-color, #2481cc);
      --tg-theme-button-text-color: var(--tg-theme-button-text-color, #ffffff);
      --tg-theme-secondary-bg-color: var(--tg-theme-secondary-bg-color, #f0f0f0);
    }

    * { margin: 0; padding: 0; box-sizing: border-box; }

    body {
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
      background: var(--tg-theme-bg-color);
      color: var(--tg-theme-text-color);
      min-height: 100vh;
      padding: 16px;
    }
  </style>
</head>
<body>
  <!-- Your app content here -->

  <script>
    const tg = window.Telegram.WebApp;
    tg.ready();
    tg.expand(); // expand to full height

    // Access theme params: tg.themeParams.bg_color, text_color, etc.
    // Access user: tg.initDataUnsafe.user (id, first_name, last_name, username)
    // Close the app: tg.close()
    // Send data to bot: tg.sendData(JSON.stringify({...})) — closes the app
    // Main button: tg.MainButton.setText('Submit').show().onClick(() => {...})
  </script>
</body>
</html>
```

## Telegram WebApp SDK reference

### Initialization
```js
const tg = window.Telegram.WebApp;
tg.ready();          // tell Telegram the app is ready
tg.expand();         // expand to full viewport height
```

### Theme
The SDK injects CSS variables matching the user's Telegram theme. Always use these for colors:
- `var(--tg-theme-bg-color)` — main background
- `var(--tg-theme-text-color)` — primary text
- `var(--tg-theme-hint-color)` — secondary/hint text
- `var(--tg-theme-link-color)` — links
- `var(--tg-theme-button-color)` — primary button background
- `var(--tg-theme-button-text-color)` — primary button text
- `var(--tg-theme-secondary-bg-color)` — card/section backgrounds

### Main button (bottom action button)
```js
tg.MainButton.setText('Save').show();
tg.MainButton.onClick(() => { /* handle tap */ });
tg.MainButton.hide();
```

### Back button
```js
tg.BackButton.show();
tg.onEvent('backButtonClicked', () => { /* navigate back or close */ });
```

### User info
```js
const user = tg.initDataUnsafe.user;
// user.id, user.first_name, user.last_name, user.username
```

### Haptic feedback
```js
tg.HapticFeedback.impactOccurred('medium');  // light, medium, heavy
tg.HapticFeedback.notificationOccurred('success');  // success, warning, error
```

### Closing
```js
tg.close();  // close the mini app
```

### Sending data back to bot
```js
tg.sendData(JSON.stringify({ action: 'submit', data: formData }));
// This closes the mini app and sends the data as a service message to the bot
```

## Data persistence

Mini apps can use `localStorage` for client-side persistence. Data persists between opens for the same user on the same device.

```js
// Save
localStorage.setItem('app-data', JSON.stringify(state));

// Load
const saved = localStorage.getItem('app-data');
if (saved) state = JSON.parse(saved);
```

For server-side persistence, write/read JSON files to `~/.tinyclaw/miniapps/{app-name}/data/`.

## Frontend design principles

Create distinctive, production-grade interfaces. Avoid generic AI aesthetics.

- **Typography**: Choose fonts that match the app's personality. Use Google Fonts via CDN. Pair a distinctive display font with a clean body font.
- **Color**: Respect Telegram theme variables as the base palette, but add accent colors and gradients that give the app character.
- **Motion**: Use CSS transitions and animations for micro-interactions. Staggered reveals on load, smooth state transitions, hover/tap feedback.
- **Layout**: Use the full viewport. Cards, grids, and lists should feel native to mobile.
- **Touch targets**: Minimum 44px for tap targets. Add haptic feedback on important actions.

## Menu button (persistent launcher)

You can pin a mini app as the chat's **menu button** — the button next to the text input field in Telegram. This replaces the default command menu and gives the user one-tap access to the mini app at all times.

Use this for mini apps the user will open frequently (daily tracker, dashboard, todo list, etc). The menu button persists across messages — it stays until explicitly changed or reset.

To set the menu button, include this tag in your response **in addition to** the `[miniapp:]` tag:

```
[menubutton: app-name: Button Text]
```

- `app-name` must match a miniapp directory under `~/.tinyclaw/miniapps/`
- `Button Text` is the label shown on the menu button (keep it short, ~2-3 words)
- The `[menubutton:]` tag is stripped from the response just like `[miniapp:]`
- You can use both tags together — `[miniapp:]` sends an inline button on the message, `[menubutton:]` pins it as the persistent menu button

To reset the menu button back to the default commands menu, use:
```
[menubutton: commands: Commands]
```

## Response format

Your response should include:
1. A brief explanation of what the mini app does
2. The `[miniapp: app-name: Button Text]` tag
3. Optionally, the `[menubutton: app-name: Button Text]` tag if the user wants persistent access

Example response:
```
I built you a nutrition tracker! It lets you log meals, see daily totals, and track macros over time. Data is saved locally on your device.

[miniapp: nutrition-tracker: Open Tracker]
[menubutton: nutrition-tracker: Nutrition]
```

## Important notes

- The mini app URL must be HTTPS (tinyclaw handles this via the Blaxel preview URL)
- Mini apps open in Telegram's embedded browser — test for mobile viewport
- Always call `tg.ready()` and `tg.expand()` at startup
- Always use `var(--tg-theme-*)` CSS variables for colors — this ensures the app matches the user's Telegram theme (light/dark mode)
- Use `<meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no">` for proper mobile scaling
- Keep external dependencies minimal — prefer inline CSS/JS, use CDN for fonts/icons only
- For multi-file apps, all files must be under `~/.tinyclaw/miniapps/{app-name}/`
