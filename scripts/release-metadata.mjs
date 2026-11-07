import fs from 'node:fs';
import crypto from 'node:crypto';

// Release URLs follow the repository that builds them, so forks publish
// manifests pointing at their own releases.
const repository = process.env.GITHUB_REPOSITORY || 'Nikolay1243/Webos-EARC-Volume-Overlay';
const branch = process.env.RELEASE_BRANCH || 'main';
const version = JSON.parse(fs.readFileSync('app/appinfo.json')).version;
const base = `https://github.com/${repository}/releases/download/v${version}`;
fs.mkdirSync('build/release', { recursive: true });
fs.rmSync('build/release/com.github.nikolay1243.earcvolume.settings.manifest.json', { force: true });
const packages = [];
for (const directory of ['app']) {
  const app = JSON.parse(fs.readFileSync(`${directory}/appinfo.json`));
  const filename = `${app.id}_${app.version}_all.ipk`;
  const data = fs.readFileSync(`build/${filename}`);
  const iconUri = `https://raw.githubusercontent.com/${repository}/${branch}/${directory}/${app.largeIcon}`;
  const manifest = {
    id: app.id, version: app.version, type: 'web',
    title: app.title,
    appDescription: app.appDescription,
    iconUri,
    sourceUrl: `https://github.com/${repository}`, rootRequired: true,
    ipkUrl: `${base}/${filename}`,
    ipkHash: { sha256: crypto.createHash('sha256').update(data).digest('hex') },
    ipkSize: data.length
  };
  const manifestName = `${app.id}.manifest.json`;
  fs.writeFileSync(`build/release/${manifestName}`, JSON.stringify(manifest, null, 2) + '\n');
  packages.push({ id: app.id, title: app.title, iconUri, manifestUrl: `${base}/${manifestName}` });
}
// Homebrew Channel repository index for this release.
const index = {
  paging: { page: 1, count: packages.length, maxPage: 1, itemsTotal: packages.length },
  packages
};
fs.writeFileSync('build/release/repo.json', JSON.stringify(index, null, 2) + '\n');
