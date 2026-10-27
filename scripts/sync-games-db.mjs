// Syncs the games database from Discord's public detectable applications endpoint
// and merges it with existing curated entries (preserving any manual title regexes/overrides).

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { formatGames } from './format-games-db.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const gamesJsonPath = path.resolve(__dirname, '../src-tauri/src/activity/games.json');

async function sync() {
  console.log('Fetching Discord detectable applications...');
  const res = await fetch('https://discord.com/api/v9/applications/detectable');
  if (!res.ok) {
    throw new Error(`Failed to fetch detectable applications: ${res.statusText}`);
  }
  const discordApps = await res.json();
  console.log(`Fetched ${discordApps.length} applications from Discord.`);

  let existing = [];
  if (fs.existsSync(gamesJsonPath)) {
    existing = JSON.parse(fs.readFileSync(gamesJsonPath, 'utf8'));
  }

  const existingExes = new Set();
  for (const item of existing) {
    for (const exe of item.exes || []) {
      existingExes.add(exe.toLowerCase());
    }
  }

  let addedCount = 0;
  for (const app of discordApps) {
    const name = app.name?.trim();
    if (!name) continue;

    const exes = [];
    for (const e of app.executables || []) {
      if (e.os === 'win32' && e.name && !e.is_launcher) {
        const lower = e.name.toLowerCase();
        if (!existingExes.has(lower)) {
          exes.push(e.name);
          existingExes.add(lower);
        }
      }
    }

    if (exes.length > 0) {
      existing.push({ name, exes });
      addedCount++;
    }
  }

  fs.writeFileSync(gamesJsonPath, formatGames(existing), 'utf8');
  console.log(`Saved ${existing.length} games to games.json (+${addedCount} new).`);
}

sync().catch((err) => {
  console.error(err);
  process.exit(1);
});
