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
```

The output can be found in `release/`.
