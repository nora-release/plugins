import { createHash } from 'node:crypto'
import { mkdir, readFile, stat, writeFile } from 'node:fs/promises'
import { dirname, posix, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { inflateRawSync } from 'node:zlib'

export type PluginManifest = {
  schemaVersion: 1; apiVersion: 1; id: string; name: string; version: string; summary: string; publisher: string
  extensionBundleIdentifier: string; permissions: string[]
  panels: { id: string; title: string; systemImage: string }[]
  commands: { id: string; title: string; description: string; inputSchemaJSON: string; readOnly: boolean }[]
}
export type RegistryEntry = {
  manifest: PluginManifest; downloadURL: string; sha256: string
  repositoryURL: string; releaseTag: string; teamIdentifier: string
}
export type Registry = { schemaVersion: 1; name: string; plugins: RegistryEntry[] }
const maximumArchiveBytes = 256 * 1024 * 1024
const maximumManifestBytes = 512 * 1024
const identifier = /^[A-Za-z0-9][A-Za-z0-9-]*(?:\.[A-Za-z0-9][A-Za-z0-9-]*)+$/
const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/

function requireValue(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message)
}
function object(value: unknown): Record<string, unknown> {
  requireValue(value !== null && typeof value === 'object' && !Array.isArray(value), 'Expected a JSON object')
  return value as Record<string, unknown>
}
function text(value: unknown, field: string, maximum = 128): asserts value is string {
  requireValue(typeof value === 'string' && value.trim().length > 0 && Buffer.byteLength(value) <= maximum
    && !/\p{Cc}/u.test(value), `${field} must be nonempty plain text of at most ${maximum} bytes`)
}
function id(value: unknown, field: string): asserts value is string {
  text(value, field, 255)
  requireValue(identifier.test(value), `${field} must be a dot-separated identifier`)
}
function array(value: unknown, field: string, maximum: number): unknown[] {
  requireValue(Array.isArray(value) && value.length <= maximum, `${field} exceeds its limit or is not an array`)
  return value
}

export function validateManifest(value: unknown): PluginManifest {
  const manifest = object(value)
  requireValue(manifest.schemaVersion === 1 && manifest.apiVersion === 1, 'Only plugin schemaVersion 1 and apiVersion 1 are supported')
  id(manifest.id, 'id'); id(manifest.extensionBundleIdentifier, 'extensionBundleIdentifier')
  text(manifest.name, 'name'); text(manifest.publisher, 'publisher'); text(manifest.summary, 'summary', 2048)
  text(manifest.version, 'version')
  requireValue(semver.test(manifest.version), 'version must be a strict semantic version')
  const permissions = array(manifest.permissions, 'permissions', 2)
  requireValue(new Set(permissions).size === permissions.length && permissions.every(value => value === 'hostInfo' || value === 'notifications'), 'Unknown or duplicate plugin permission')
  const contributions = new Set<string>()
  const contribution = (value: unknown) => {
    const item = object(value)
    id(item.id, 'contribution id')
    requireValue(item.id.startsWith(`${manifest.id}.`) && !contributions.has(item.id), 'Contribution IDs must be unique and belong to the plugin')
    contributions.add(item.id)
    text(item.title, 'contribution title')
    return item
  }
  for (const panel of array(manifest.panels, 'panels', 32)) text(contribution(panel).systemImage, 'panel systemImage')
  for (const command of array(manifest.commands, 'commands', 128)) {
    const item = contribution(command)
    text(item.description, 'command description', 4096)
    requireValue(typeof item.inputSchemaJSON === 'string' && Buffer.byteLength(item.inputSchemaJSON) <= 64 * 1024, 'Command schema exceeds 64 KiB or is not a string')
    requireValue(object(JSON.parse(item.inputSchemaJSON)).type === 'object', 'Command input schema must describe a JSON object')
    requireValue(typeof item.readOnly === 'boolean', 'Command readOnly must be a boolean')
  }
  requireValue(Buffer.byteLength(JSON.stringify(manifest)) <= maximumManifestBytes, 'Plugin manifest exceeds 512 KiB')
  return manifest as PluginManifest
}

export function repositoryPath(value: unknown): string {
  requireValue(typeof value === 'string', 'repositoryURL is required for public plugins')
  const url = new URL(value)
  requireValue(url.protocol === 'https:' && url.hostname === 'github.com' && !url.port && !url.username && !url.password
    && !url.search && !url.hash && /^\/[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9][A-Za-z0-9._-]*$/.test(url.pathname)
    && !url.pathname.endsWith('.git'), 'repositoryURL must be a canonical public https://github.com/owner/repository URL')
  return url.pathname.slice(1)
}

export function validateEntry(value: unknown): RegistryEntry {
  const entry = object(value)
  const manifest = validateManifest(entry.manifest)
  const repository = repositoryPath(entry.repositoryURL)
  requireValue(entry.releaseTag === manifest.version || entry.releaseTag === `v${manifest.version}`, 'releaseTag must match the exact manifest version, optionally prefixed by v')
  requireValue(typeof entry.teamIdentifier === 'string' && /^[A-Z0-9]{10}$/.test(entry.teamIdentifier), 'teamIdentifier must be a 10-character Apple Developer Team ID')
  requireValue(typeof entry.sha256 === 'string' && /^[a-f0-9]{64}$/.test(entry.sha256) && !/^0+$/.test(entry.sha256), 'sha256 must be a real lowercase SHA-256 digest')
  requireValue(typeof entry.downloadURL === 'string', 'downloadURL is required')
  const url = new URL(entry.downloadURL)
  const prefix = `/${repository}/releases/download/${encodeURIComponent(entry.releaseTag)}/`
  requireValue(url.protocol === 'https:' && url.hostname === 'github.com' && !url.port && !url.username && !url.password
    && !url.search && !url.hash && url.pathname.startsWith(prefix), 'downloadURL must point to the declared repository and exact release tag')
  const asset = decodeURIComponent(url.pathname.slice(prefix.length))
  requireValue(/^[A-Za-z0-9][A-Za-z0-9._-]*\.zip$/.test(asset), 'Release asset must be a ZIP with a simple filename')
  return { manifest, repositoryURL: entry.repositoryURL as string, releaseTag: entry.releaseTag as string,
    teamIdentifier: entry.teamIdentifier, downloadURL: entry.downloadURL, sha256: entry.sha256 }
}

export function validateRegistry(value: unknown): Registry {
  const registry = object(value)
  requireValue(registry.schemaVersion === 1, 'Only marketplace schemaVersion 1 is supported')
  text(registry.name, 'marketplace name', 200)
  const plugins = array(registry.plugins, 'plugins', 1000).map(validateEntry).sort((left, right) => left.manifest.id.localeCompare(right.manifest.id, 'en'))
  requireValue(new Set(plugins.map(entry => entry.manifest.id)).size === plugins.length, 'Duplicate plugin IDs in registry')
  return { schemaVersion: 1, name: registry.name, plugins }
}

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`
  if (value !== null && typeof value === 'object') return `{${Object.entries(value).sort(([a], [b]) => a.localeCompare(b, 'en')).map(([key, value]) => `${JSON.stringify(key)}:${canonical(value)}`).join(',')}}`
  return JSON.stringify(value)
}
export function formatRegistry(registry: Registry): string { return `${JSON.stringify(registry, null, 2)}\n` }

function crc32(data: Buffer): number {
  let result = 0xffffffff
  for (const byte of data) {
    result ^= byte
    for (let bit = 0; bit < 8; bit += 1) result = (result >>> 1) ^ ((result & 1) ? 0xedb88320 : 0)
  }
  return (result ^ 0xffffffff) >>> 0
}

export function manifestFromArchive(data: Buffer): PluginManifest {
  requireValue(data.length >= 22 && data.length <= maximumArchiveBytes, 'Invalid or oversized ZIP')
  let end = -1
  for (let offset = data.length - 22; offset >= Math.max(0, data.length - 65557); offset -= 1) {
    if (data.readUInt32LE(offset) === 0x06054b50 && offset + 22 + data.readUInt16LE(offset + 20) === data.length) { end = offset; break }
  }
  requireValue(end >= 0, 'ZIP end record is missing')
  const count = data.readUInt16LE(end + 10)
  const central = data.readUInt32LE(end + 16)
  requireValue(count > 0 && count <= 20000 && data.readUInt16LE(end + 4) === 0 && data.readUInt16LE(end + 6) === 0
    && data.readUInt16LE(end + 8) === count && central + data.readUInt32LE(end + 12) === end, 'Split, ZIP64, or oversized archives are unsupported')
  let offset = central
  let totalBytes = 0
  const names = new Set<string>()
  const roots = new Set<string>()
  const symlinks = new Set<string>()
  const paths: string[] = []
  let manifest: PluginManifest | undefined
  for (let index = 0; index < count; index += 1) {
    requireValue(offset + 46 <= end && data.readUInt32LE(offset) === 0x02014b50, 'Invalid ZIP directory')
    const flags = data.readUInt16LE(offset + 8), method = data.readUInt16LE(offset + 10)
    const checksum = data.readUInt32LE(offset + 16), compressed = data.readUInt32LE(offset + 20), size = data.readUInt32LE(offset + 24)
    const nameLength = data.readUInt16LE(offset + 28), local = data.readUInt32LE(offset + 42)
    const next = offset + 46 + nameLength + data.readUInt16LE(offset + 30) + data.readUInt16LE(offset + 32)
    requireValue(nameLength > 0 && nameLength <= 4096 && next <= end && !(flags & 1) && [0, 8].includes(method)
      && data.readUInt16LE(offset + 34) === 0, 'Encrypted or malformed ZIP entry')
    const nameBytes = data.subarray(offset + 46, offset + 46 + nameLength)
    const name = new TextDecoder('utf-8', { fatal: true }).decode(nameBytes)
    const parts = name.replace(/\/$/, '').split('/')
    requireValue(parts.every(part => part !== '' && part !== '.' && part !== '..') && !/[\\\0]/.test(name), 'Unsafe ZIP path')
    const normalized = parts.join('/').normalize('NFC').toLowerCase()
    requireValue(!names.has(normalized), 'Duplicate ZIP path')
    names.add(normalized); paths.push(normalized)
    if (parts[0] !== '__MACOSX') roots.add(parts[0])
    totalBytes += size
    requireValue(totalBytes <= 512 * 1024 * 1024 && local + 30 <= central && data.readUInt32LE(local) === 0x04034b50
      && data.readUInt16LE(local + 6) === flags && data.readUInt16LE(local + 8) === method
      && data.readUInt16LE(local + 26) === nameLength, 'Invalid ZIP local record or size limit')
    const start = local + 30 + nameLength + data.readUInt16LE(local + 28)
    requireValue(start + compressed <= central && data.subarray(local + 30, local + 30 + nameLength).equals(nameBytes), 'ZIP entry boundaries do not match')
    const mode = data.readUInt32LE(offset + 38) >>> 16, type = mode & 0o170000
    requireValue([0, 0o040000, 0o100000, 0o120000].includes(type), 'Unsupported ZIP file type')
    if (type === 0o120000) {
      requireValue(size > 0 && size <= 4096 && !name.endsWith('/'), 'Invalid ZIP symbolic link')
      const compressedData = data.subarray(start, start + compressed)
      const contents = method === 0 ? compressedData : inflateRawSync(compressedData, { maxOutputLength: 4096 })
      requireValue(contents.length === size && crc32(contents) === checksum, 'ZIP symbolic link checksum does not match')
      const target = new TextDecoder('utf-8', { fatal: true }).decode(contents)
      requireValue(!target.startsWith('/') && !/[\\\0]/.test(target), 'Unsafe ZIP symbolic link target')
      const resolved = posix.normalize(`${parts.slice(0, -1).join('/')}/${target}`)
      requireValue(resolved.startsWith(`${parts[0]}/`), 'ZIP symbolic link escapes its application')
      symlinks.add(normalized)
    }
    if (parts[0] !== '__MACOSX' && parts.slice(1).join('/') === 'Contents/Resources/nora-plugin.json') {
      requireValue(!manifest && type !== 0o120000 && !name.endsWith('/') && size <= maximumManifestBytes, 'Invalid embedded manifest')
      const compressedData = data.subarray(start, start + compressed)
      const contents = method === 0 ? compressedData : inflateRawSync(compressedData, { maxOutputLength: maximumManifestBytes })
      requireValue(contents.length === size && crc32(contents) === checksum, 'Embedded manifest checksum does not match')
      manifest = validateManifest(JSON.parse(contents.toString('utf8')))
    }
    offset = next
  }
  requireValue(offset === end && roots.size === 1 && [...roots][0].endsWith('.app') && manifest, 'ZIP must contain one .app and its Nora manifest')
  for (const path of paths) {
    const parts = path.split('/')
    for (let index = 1; index < parts.length; index += 1) requireValue(!symlinks.has(parts.slice(0, index).join('/')), 'ZIP entry passes through a symbolic link')
  }
  return manifest
}

export function verifyArchive(entry: RegistryEntry, archive: Buffer): void {
  requireValue(createHash('sha256').update(archive).digest('hex') === entry.sha256, 'Archive SHA-256 does not match registry')
  requireValue(canonical(manifestFromArchive(archive)) === canonical(entry.manifest), 'Archive manifest does not match registry')
}

export async function downloadRelease(url: string, fetcher: typeof fetch = fetch): Promise<Buffer> {
  for (let redirects = 0; redirects <= 5; redirects += 1) {
    const target = new URL(url)
    requireValue(target.protocol === 'https:' && !target.username && !target.password && !target.port
      && ['github.com', 'release-assets.githubusercontent.com', 'objects.githubusercontent.com'].includes(target.hostname), 'Unexpected release download host')
    const response = await fetcher(target, { redirect: 'manual', signal: AbortSignal.timeout(180000) })
    if ([301, 302, 303, 307, 308].includes(response.status)) {
      const location = response.headers.get('location')
      requireValue(location, 'Download redirect has no location')
      await response.body?.cancel()
      url = new URL(location, target).href
      continue
    }
    requireValue(response.status === 200 && response.body, `Release download returned HTTP ${response.status}`)
    requireValue(Number(response.headers.get('content-length') ?? 0) <= maximumArchiveBytes, 'Release archive exceeds 256 MiB')
    const chunks: Buffer[] = []
    let size = 0
    const reader = response.body.getReader()
    try {
      while (true) {
        const chunk = await reader.read()
        if (chunk.done) break
        size += chunk.value.byteLength
        requireValue(size <= maximumArchiveBytes, 'Release archive exceeds 256 MiB')
        chunks.push(Buffer.from(chunk.value))
      }
    } finally { await reader.cancel() }
    return Buffer.concat(chunks)
  }
  throw new Error('Too many release download redirects')
}

export async function verifyDownloads(registry: Registry, baseline?: Registry, fetcher: typeof fetch = fetch): Promise<void> {
  const previous = new Set(baseline?.plugins.map(canonical) ?? [])
  const changed = registry.plugins.filter(entry => !previous.has(canonical(entry)))
  requireValue(changed.length <= 100, 'Validate at most 100 new or changed plugins per submission')
  let total = 0
  for (const entry of changed) {
    const archive = await downloadRelease(entry.downloadURL, fetcher)
    total += archive.length
    requireValue(total <= 1024 * 1024 * 1024, 'Submission downloads exceed 1 GiB')
    verifyArchive(entry, archive)
  }
}

async function readJSON(path: string, limit = 4 * 1024 * 1024): Promise<unknown> {
  requireValue((await stat(path)).size <= limit, 'JSON file exceeds size limit')
  return JSON.parse(await readFile(path, 'utf8'))
}

async function main(args: string[]): Promise<void> {
  const [command, ...rest] = args
  const flags = new Map<string, string>()
  for (let index = 0; index < rest.length; index += 1) {
    const key = rest[index]
    requireValue(key.startsWith('--') && !flags.has(key), `Invalid or duplicate option: ${key}`)
    if (key === '--verify-downloads') flags.set(key, 'true')
    else { requireValue(rest[index + 1] && !rest[index + 1].startsWith('--'), `Missing value for ${key}`); flags.set(key, rest[++index]) }
  }
  const required = (key: string) => { const value = flags.get(key); requireValue(value, `Missing ${key}`); return value }
  if (command === 'validate-manifest') {
    validateManifest(await readJSON(required('--manifest'), maximumManifestBytes))
    console.log('Validated native plugin manifest')
    return
  }
  if (command === 'generate' || command === 'validate') {
    const registry = validateRegistry(await readJSON(required('--registry')))
    if (flags.has('--verify-downloads')) await verifyDownloads(registry, flags.has('--baseline') ? validateRegistry(await readJSON(required('--baseline'))) : undefined)
    const output = formatRegistry(registry)
    if (command === 'generate') await writeFile(required('--output'), output)
    else requireValue(await readFile(required('--catalog'), 'utf8') === output, 'marketplace.json is stale; regenerate it from registry.json')
    console.log(`Validated ${registry.plugins.length} plugin entries`)
    return
  }
  if (command === 'release-entry') {
    const manifest = validateManifest(await readJSON(required('--manifest'), maximumManifestBytes))
    const archivePath = required('--archive')
    requireValue((await stat(archivePath)).size <= maximumArchiveBytes, 'Release archive exceeds 256 MiB')
    const archive = await readFile(archivePath)
    const filename = archivePath.split(/[\\/]/).at(-1)!
    const repository = repositoryPath(required('--repository'))
    const tag = required('--tag')
    const entry = validateEntry({ manifest, repositoryURL: `https://github.com/${repository}`, releaseTag: tag,
      teamIdentifier: required('--team'), sha256: createHash('sha256').update(archive).digest('hex'),
      downloadURL: `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(filename)}` })
    verifyArchive(entry, archive)
    const directory = resolve(required('--output'))
    await mkdir(directory, { recursive: true })
    await writeFile(resolve(directory, 'marketplace.json'), formatRegistry({ schemaVersion: 1, name: manifest.name, plugins: [entry] }), { flag: 'wx' })
    await writeFile(resolve(directory, 'nora-plugin.json'), `${JSON.stringify(manifest, null, 2)}\n`, { flag: 'wx' })
    await writeFile(resolve(directory, 'SHA256SUMS'), `${entry.sha256}  ${filename}\n`, { flag: 'wx' })
    console.log(`Prepared release metadata for ${manifest.id}@${manifest.version}`)
    return
  }
  if (command === 'init-registry' || command === 'init-author') {
    const directory = resolve(required('--directory'))
    const sourceRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
    const template = resolve(sourceRoot, 'packages/plugin-kit/templates', command === 'init-registry' ? 'registry' : 'author')
    const files = command === 'init-registry'
      ? ['registry.json', 'marketplace.json', '.github/workflows/validate-marketplace.yml']
      : ['plugin-release.json', '.github/workflows/release-plugin.yml']
    for (const file of files) {
      const destination = resolve(directory, file)
      await mkdir(dirname(destination), { recursive: true })
      await writeFile(destination, await readFile(resolve(template, file)), { flag: 'wx' })
    }
    await mkdir(resolve(directory, 'scripts'), { recursive: true })
    await writeFile(resolve(directory, 'scripts/plugin-marketplace.ts'), await readFile(fileURLToPath(import.meta.url)), { flag: 'wx' })
    if (command === 'init-author') {
      for (const file of ['publish-native-plugin.sh', 'build-distribution.py']) {
        await writeFile(resolve(directory, 'scripts', file), await readFile(resolve(sourceRoot, 'packages/plugin-kit/scripts', file)), { flag: 'wx', mode: 0o755 })
      }
    }
    console.log(`Created ${command === 'init-registry' ? 'registry' : 'author release'} files in ${directory}`)
    return
  }
  throw new Error('Commands: generate, validate, validate-manifest, release-entry, init-registry, init-author')
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch(error => { console.error(error instanceof Error ? error.message : 'Marketplace operation failed'); process.exitCode = 1 })
}
