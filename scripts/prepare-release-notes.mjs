#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';

const INSTALLATION_NOTES = `## Installation

**macOS:**
- Download the \`.dmg\` file
- Drag to Applications
- On first launch: System Settings > Privacy > Allow

**Windows:**
- Download the \`.msi\` file
- Run the installer

**Linux:**
- \`.deb\` for Debian/Ubuntu: \`sudo dpkg -i voicetextai_*.deb\`
- \`.AppImage\` for other distros: make executable and run`;

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function normalizeVersion(value) {
  return String(value ?? '').trim().replace(/^refs\/tags\//, '').replace(/^v/i, '');
}

function isVersionLike(value) {
  return /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(value);
}

function resolveVersion(argv) {
  const explicit = argv.find((arg) => !arg.startsWith('-'));
  const explicitVersion = normalizeVersion(explicit);
  if (isVersionLike(explicitVersion)) return explicitVersion;

  const refVersion = normalizeVersion(process.env.GITHUB_REF_NAME);
  if (isVersionLike(refVersion)) return refVersion;

  const packageJsonPath = path.resolve(process.cwd(), 'package.json');
  const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, 'utf8'));
  return normalizeVersion(packageJson.version);
}

function extractChangelogSection(changelog, version) {
  const headerRe = new RegExp(
    String.raw`^##\s*(?:\[\s*)?${escapeRegExp(version)}(?:\s*\])?(?:\s*(?:\u2014|-).*)?\s*$`,
    'm'
  );
  const headerMatch = changelog.match(headerRe);
  if (!headerMatch || headerMatch.index == null) return null;

  const afterHeaderIdx = changelog.indexOf('\n', headerMatch.index);
  const contentStart = afterHeaderIdx === -1 ? changelog.length : afterHeaderIdx + 1;
  const remainder = changelog.slice(contentStart);
  const nextHeaderMatch = remainder.match(/^##\s+/m);
  const contentEnd = nextHeaderMatch?.index != null ? contentStart + nextHeaderMatch.index : changelog.length;
  const section = changelog.slice(contentStart, contentEnd).trim();
  return section || null;
}

function normalizeSection(section) {
  return section
    .split(/\r?\n/)
    .map((line) => line.trimEnd())
    .filter((line) => line.trim() !== '---')
    .join('\n')
    .trim();
}

const version = resolveVersion(process.argv.slice(2));
if (!version) {
  console.error('Release notes check failed: version is empty.');
  process.exit(1);
}

const changelogPath = path.resolve(process.cwd(), 'CHANGELOG.md');
const changelog = fs.readFileSync(changelogPath, 'utf8');
const section = extractChangelogSection(changelog, version);
const notes = section ? normalizeSection(section) : '';

if (!notes) {
  console.error(`Release notes check failed: CHANGELOG.md has no section for ${version}.`);
  console.error(`Add a section like "## [${version}] - YYYY-MM-DD" before tagging the release.`);
  process.exit(1);
}

process.stdout.write(`${notes}\n\n${INSTALLATION_NOTES}\n`);
