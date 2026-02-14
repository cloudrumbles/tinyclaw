#!/usr/bin/env bash
# Daemon lifecycle management for TinyClaw
# Handles starting, stopping, restarting, and status checking

PID_FILE="$HOME/.tinyclaw/daemon.pid"

# Start daemon
start_daemon() {
    if session_exists; then
        echo -e "${YELLOW}Daemon already running${NC}"
        return 1
    fi

    log "Starting TinyClaw daemon..."

    # Check if dependencies are installed
    if [ ! -d "$SCRIPT_DIR/node_modules" ]; then
        echo -e "${YELLOW}Installing dependencies...${NC}"
        cd "$SCRIPT_DIR"
        PUPPETEER_SKIP_DOWNLOAD=true bun install
    fi

    # Build TypeScript if any src file is newer than its dist counterpart
    local needs_build=false
    if [ ! -d "$SCRIPT_DIR/dist" ]; then
        needs_build=true
    else
        for ts_file in "$SCRIPT_DIR"/src/*.ts; do
            local js_file="$SCRIPT_DIR/dist/$(basename "${ts_file%.ts}.js")"
            if [ ! -f "$js_file" ] || [ "$ts_file" -nt "$js_file" ]; then
                needs_build=true
                break
            fi
        done
    fi
    if [ "$needs_build" = true ]; then
        echo -e "${YELLOW}Building TypeScript...${NC}"
        cd "$SCRIPT_DIR"
        bun run build
    fi

    # Load settings or run setup wizard
    if ! load_settings; then
        echo -e "${YELLOW}No configuration found. Running setup wizard...${NC}"
        echo ""
        "$SCRIPT_DIR/lib/setup-wizard.sh"

        if ! load_settings; then
            echo -e "${RED}Setup failed or was cancelled${NC}"
            return 1
        fi
    fi

    if [ ${#ACTIVE_CHANNELS[@]} -eq 0 ]; then
        echo -e "${RED}No channels configured. Run 'tinyclaw setup' to reconfigure${NC}"
        return 1
    fi

    # Validate tokens for channels that need them
    for ch in "${ACTIVE_CHANNELS[@]}"; do
        local token_key="${CHANNEL_TOKEN_KEY[$ch]:-}"
        if [ -n "$token_key" ] && [ -z "${CHANNEL_TOKENS[$ch]:-}" ]; then
            echo -e "${RED}${CHANNEL_DISPLAY[$ch]} is configured but bot token is missing${NC}"
            echo "Run 'tinyclaw setup' to reconfigure"
            return 1
        fi
    done

    # Check for updates (non-blocking)
    local update_info
    update_info=$(check_for_updates 2>/dev/null || true)
    if [ -n "$update_info" ]; then
        IFS='|' read -r current latest <<< "$update_info"
        show_update_notification "$current" "$latest"
    fi

    # Report channels
    echo -e "${BLUE}Channels:${NC}"
    for ch in "${ACTIVE_CHANNELS[@]}"; do
        echo -e "  ${GREEN}✓${NC} ${CHANNEL_DISPLAY[$ch]}"
    done
    echo ""

    # Launch the daemon process in the background
    nohup bun "$SCRIPT_DIR/dist/daemon.js" >> "$LOG_DIR/daemon.log" 2>&1 &
    local daemon_pid=$!

    # Wait briefly and verify it started
    sleep 2
    if ! kill -0 "$daemon_pid" 2>/dev/null; then
        echo -e "${RED}Daemon failed to start. Check logs:${NC}"
        echo "  tail -f $LOG_DIR/daemon.log"
        return 1
    fi

    echo -e "${GREEN}✓ TinyClaw started${NC}"
    echo ""

    # Build channel names for help line
    local channel_names
    channel_names=$(IFS='|'; echo "${ACTIVE_CHANNELS[*]}")

    echo -e "${GREEN}Commands:${NC}"
    echo "  Status:  tinyclaw status"
    echo "  Logs:    tinyclaw logs [$channel_names|queue|daemon]"
    echo ""

    local ch_list
    ch_list=$(IFS=','; echo "${ACTIVE_CHANNELS[*]}")
    log "Daemon started (pid=$daemon_pid, channels=$ch_list)"
}

# Stop daemon
stop_daemon() {
    log "Stopping TinyClaw..."

    # Send SIGTERM to daemon process via PID file
    if [ -f "$PID_FILE" ]; then
        local pid
        pid=$(cat "$PID_FILE")
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid"
            # Wait up to 10 seconds for graceful shutdown
            local waited=0
            while kill -0 "$pid" 2>/dev/null && [ $waited -lt 10 ]; do
                sleep 1
                waited=$((waited + 1))
            done
            # Force kill if still alive
            if kill -0 "$pid" 2>/dev/null; then
                kill -9 "$pid" 2>/dev/null || true
            fi
        fi
        rm -f "$PID_FILE"
    fi

    # Kill any remaining child processes
    for ch in "${ALL_CHANNELS[@]}"; do
        pkill -f "${CHANNEL_SCRIPT[$ch]}" 2>/dev/null || true
    done
    pkill -f "dist/queue-processor.js" 2>/dev/null || true
    pkill -f "heartbeat-cron.sh" 2>/dev/null || true
    pkill -f "dist/daemon.js" 2>/dev/null || true

    echo -e "${GREEN}✓ TinyClaw stopped${NC}"
    log "Daemon stopped"
}

# Restart daemon
restart_daemon() {
    stop_daemon
    sleep 2
    start_daemon
}

# Status
status_daemon() {
    echo -e "${BLUE}TinyClaw Status${NC}"
    echo "==============="
    echo ""

    if session_exists; then
        local pid
        pid=$(cat "$PID_FILE" 2>/dev/null)
        echo -e "Daemon:          ${GREEN}Running${NC} (pid=$pid)"
    else
        echo -e "Daemon:          ${RED}Not Running${NC}"
        echo "  Start: tinyclaw start"
    fi

    echo ""

    # Channel process status
    for ch in "${ALL_CHANNELS[@]}"; do
        local display="${CHANNEL_DISPLAY[$ch]}"
        local script="${CHANNEL_SCRIPT[$ch]}"
        local pad=""
        # Pad display name to align output
        while [ $((${#display} + ${#pad})) -lt 16 ]; do pad="$pad "; done

        if pgrep -f "$script" > /dev/null; then
            echo -e "${display}:${pad}${GREEN}Running${NC}"
        else
            echo -e "${display}:${pad}${RED}Not Running${NC}"
        fi
    done

    # Core processes
    if pgrep -f "dist/queue-processor.js" > /dev/null; then
        echo -e "Queue Processor: ${GREEN}Running${NC}"
    else
        echo -e "Queue Processor: ${RED}Not Running${NC}"
    fi

    if pgrep -f "heartbeat-cron.sh" > /dev/null; then
        echo -e "Heartbeat:       ${GREEN}Running${NC}"
    else
        echo -e "Heartbeat:       ${RED}Not Running${NC}"
    fi

    # Recent activity per channel (only show if log file exists)
    for ch in "${ALL_CHANNELS[@]}"; do
        if [ -f "$LOG_DIR/${ch}.log" ]; then
            echo ""
            echo "Recent ${CHANNEL_DISPLAY[$ch]} Activity:"
            printf '%0.s─' {1..24}; echo ""
            tail -n 5 "$LOG_DIR/${ch}.log"
        fi
    done

    echo ""
    echo "Recent Heartbeats:"
    printf '%0.s─' {1..18}; echo ""
    tail -n 3 "$LOG_DIR/heartbeat.log" 2>/dev/null || echo "  No heartbeat logs yet"

    echo ""
    echo "Logs:"
    for ch in "${ALL_CHANNELS[@]}"; do
        local display="${CHANNEL_DISPLAY[$ch]}"
        local pad=""
        while [ $((${#display} + ${#pad})) -lt 10 ]; do pad="$pad "; done
        echo "  ${display}:${pad}tail -f $LOG_DIR/${ch}.log"
    done
    echo "  Heartbeat: tail -f $LOG_DIR/heartbeat.log"
    echo "  Daemon:    tail -f $LOG_DIR/daemon.log"
}
