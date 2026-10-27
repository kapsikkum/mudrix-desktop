// One game per line, so the list is readable, diffs well and is easy to edit by hand.
// The build (src-tauri/build.rs) minifies it into the binary. Run with no arguments to
// re-format games.json after editing it.
import fs from 'node:fs';

export function formatGames(games) {
  const lines = games.map((game) => {
    const fields = Object.entries(game).map(([key, value]) => `"${key}": ${JSON.stringify(value).replace(/","/g, '", "')}`);
    return `  { ${fields.join(', ')} }`;
  });
  return `[\n${lines.join(',\n')}\n]\n`;
}

if (process.argv[1]?.endsWith('format-games-db.mjs')) {
  const file = new URL('../src-tauri/src/activity/games.json', import.meta.url);
  fs.writeFileSync(file, formatGames(JSON.parse(fs.readFileSync(file, 'utf8'))), 'utf8');
}
