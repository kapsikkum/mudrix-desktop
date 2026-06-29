# WizardChat Desktop

This is the desktop app for [WizardChat](https://wizardchat.org), built using Electron.

The main repo can be found at [WizardChat-Web](https://gitlab.com/wizardchat/wizardchat-web).

## Setup

**Clone the repo with submodules**

```sh
git clone --recurse-submodules https://gitlab.com/wizardchat/wizardchat-desktop.git
cd wizardchat-desktop
```

If you already cloned without submodules:

```sh
git submodule update --init --recursive
```

**Install dependencies** 

```sh
npm install
```

---

## Build

```sh
# build for macOS
npm run package:mac

# build for linux (AppImage)
npm run package:linux

# build for windows
npm run package:win
```

The output can be found in `release/`.

---

## macOS: Override unsigned build warning

On macOS, WizardChat is not officially signed. The first time you open it, macOS may block the app. To allow it:

1. Open **System Settings** → **Privacy & Security**
2. Scroll to the bottom under **Security**
3. Click **Allow WizardChat**
