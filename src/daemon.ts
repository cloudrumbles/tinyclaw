import fs from 'fs';
import path from 'path';
import { spawn, ChildProcess } from 'child_process';
import { TINYCLAW_HOME, SCRIPT_DIR, getSettings } from './lib/config';

const LOG_DIR = path.join(TINYCLAW_HOME, 'logs');
const PID_FILE = path.join(TINYCLAW_HOME, 'daemon.pid');

// Channel metadata — mirrors lib/common.sh registry
const CHANNEL_SCRIPTS: Record<string, string> = {
    discord: 'dist/channels/discord-client.js',
    whatsapp: 'dist/channels/whatsapp-client.js',
    telegram: 'dist/channels/telegram-client.js',
};

const CHANNEL_TOKEN_ENV: Record<string, string> = {
    discord: 'DISCORD_BOT_TOKEN',
    telegram: 'TELEGRAM_BOT_TOKEN',
};

interface ManagedProcess {
    name: string;
    command: string;
    args: string[];
    process: ChildProcess | null;
    restartCount: number;
    lastRestart: number;
}

const children: ManagedProcess[] = [];
let shuttingDown = false;

function log(msg: string) {
    const line = `[${new Date().toISOString()}] [daemon] ${msg}`;
    console.log(line);
    try {
        fs.appendFileSync(path.join(LOG_DIR, 'daemon.log'), line + '\n');
    } catch {}
}

function writeEnvFile(settings: ReturnType<typeof getSettings>) {
    const channels = settings.channels;
    if (!channels) return;

    const enabled: string[] = channels.enabled || [];
    const lines: string[] = [];

    for (const ch of enabled) {
        const envVar = CHANNEL_TOKEN_ENV[ch];
        const token = (channels as any)[ch]?.bot_token;
        if (envVar && token) {
            lines.push(`${envVar}=${token}`);
        }
    }

    fs.writeFileSync(path.join(SCRIPT_DIR, '.env'), lines.join('\n') + '\n');
}

function spawnChild(managed: ManagedProcess) {
    const env = { ...process.env };
    delete env.CLAUDECODE;

    const logFile = path.join(LOG_DIR, `${managed.name}.log`);
    const logFd = fs.openSync(logFile, 'a');

    // Children write to stdout, daemon routes to log files
    const child = spawn(managed.command, managed.args, {
        cwd: SCRIPT_DIR,
        stdio: ['ignore', logFd, logFd],
        env,
    });

    managed.process = child;

    child.on('exit', (code, signal) => {
        try { fs.closeSync(logFd); } catch {}

        if (shuttingDown) return;

        log(`${managed.name} exited (code=${code}, signal=${signal})`);

        // Restart with backoff
        const now = Date.now();
        const timeSinceLastRestart = now - managed.lastRestart;

        if (timeSinceLastRestart < 5000) {
            managed.restartCount++;
        } else {
            managed.restartCount = 0;
        }

        // Cap backoff at 30 seconds
        const delay = Math.min(1000 * Math.pow(2, managed.restartCount), 30000);
        log(`restarting ${managed.name} in ${delay}ms (attempt ${managed.restartCount + 1})`);

        setTimeout(() => {
            if (!shuttingDown) {
                managed.lastRestart = Date.now();
                spawnChild(managed);
            }
        }, delay);
    });

    log(`started ${managed.name} (pid=${child.pid})`);
}

function shutdown() {
    if (shuttingDown) return;
    shuttingDown = true;
    log('shutting down...');

    for (const child of children) {
        if (child.process && !child.process.killed) {
            child.process.kill('SIGTERM');
        }
    }

    // Give children 5 seconds to exit, then force kill
    setTimeout(() => {
        for (const child of children) {
            if (child.process && !child.process.killed) {
                child.process.kill('SIGKILL');
            }
        }

        try { fs.unlinkSync(PID_FILE); } catch {}
        log('stopped');
        process.exit(0);
    }, 5000);
}

function main() {
    fs.mkdirSync(LOG_DIR, { recursive: true });

    // Write PID file
    fs.writeFileSync(PID_FILE, String(process.pid));
    log(`daemon started (pid=${process.pid})`);

    // Load settings
    const settings = getSettings();
    const enabled: string[] = settings.channels?.enabled || [];

    if (enabled.length === 0) {
        log('no channels configured — exiting');
        fs.unlinkSync(PID_FILE);
        process.exit(1);
    }

    // Write .env for channel clients
    writeEnvFile(settings);

    // Spawn channel clients
    for (const ch of enabled) {
        const script = CHANNEL_SCRIPTS[ch];
        if (!script) {
            log(`unknown channel: ${ch} — skipping`);
            continue;
        }

        const managed: ManagedProcess = {
            name: ch,
            command: 'bun',
            args: [script],
            process: null,
            restartCount: 0,
            lastRestart: Date.now(),
        };
        children.push(managed);
        spawnChild(managed);
    }

    // Spawn queue processor
    const queue: ManagedProcess = {
        name: 'queue',
        command: 'bun',
        args: ['dist/queue-processor.js'],
        process: null,
        restartCount: 0,
        lastRestart: Date.now(),
    };
    children.push(queue);
    spawnChild(queue);

    // Spawn heartbeat
    const heartbeat: ManagedProcess = {
        name: 'heartbeat',
        command: 'bash',
        args: ['lib/heartbeat-cron.sh'],
        process: null,
        restartCount: 0,
        lastRestart: Date.now(),
    };
    children.push(heartbeat);
    spawnChild(heartbeat);

    // Handle signals
    process.on('SIGTERM', shutdown);
    process.on('SIGINT', shutdown);

    log(`managing ${children.length} processes`);
}

main();
