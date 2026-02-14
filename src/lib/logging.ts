export function log(level: string, message: string): void {
    const timestamp = new Date().toISOString();
    console.log(`[${timestamp}] [${level}] ${message}`);
}

// no-op — visualizer removed
export function emitEvent(_type: string, _data: Record<string, unknown>): void {}
