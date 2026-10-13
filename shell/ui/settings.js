// DigUp's Settings window: opening at login, centering the search panel where the desktop places windows itself (it
// costs memory, so it's a choice), and where the shortcut lives. The shell's settings_info and set_* commands.
"use strict";

const tauri = window.__TAURI__;
const invoke = (command, args) => tauri.core.invoke(command, args);
const $ = (id) => document.getElementById(id);

const notes = {
  gnome: "DigUp added it to GNOME's keyboard shortcuts. To change it: Settings → Keyboard → View and Customize " +
    "Shortcuts → Custom Shortcuts.",
  kde: "DigUp added it to KDE's shortcuts. To change it: System Settings → Shortcuts → DigUp.",
  by_hand: "This desktop doesn't let apps set their own shortcuts. To open the panel with one, add a shortcut in its " +
    "keyboard settings that runs:",
};

async function start() {
  const info = await invoke("settings_info");
  $("login-option").hidden = !info.canOpenAtLogin;
  $("login").checked = info.openAtLogin;
  $("center-option").hidden = !info.canCenter;
  $("center").checked = info.centered;
  $("keys").replaceChildren(...info.hotkey.split("+").filter(Boolean).map((key) => {
    const cap = document.createElement("span");
    cap.className = "key";
    cap.textContent = key;
    return cap;
  }));
  const note = notes[info.shortcut.method];
  $("shortcut-note").hidden = !note;
  $("shortcut-note").textContent = note || "";
  $("shortcut-command").hidden = info.shortcut.method !== "by_hand";
  $("shortcut-command").textContent = info.shortcut.command;
}

/// A switch: the shell does it, or the box goes back and says why.
function wire(box, command) {
  box.addEventListener("change", async () => {
    $("problem").hidden = true;
    try {
      await invoke(command, { on: box.checked });
    } catch (error) {
      box.checked = !box.checked;
      $("problem").textContent = String(error);
      $("problem").hidden = false;
    }
  });
}

wire($("login"), "set_open_at_login");
wire($("center"), "set_center_panel");
start();
