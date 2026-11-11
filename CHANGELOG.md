# Changelog

*[Français](CHANGELOG.fr.md)*

All notable changes to sshOs. Versions follow [SemVer](https://semver.org/).

## [0.5.0] - 2026-11-11

### New

#### 🗺️ New Homelab app: an interactive map of the server
The server drawn as a homelab map, built from live data:
- **Hardware**: CPU, memory, OS, IP addresses, Docker version.
- **Disks** and the filesystems Docker stores its data on, with usage and the containers using each one.
- **Docker networks**, **ports open** on the network and **proxy routes** (nginx and Nginx Proxy Manager).
- **Services** in columns by category (Edge & security, Web apps, Databases, Media, Productivity, Monitoring, Dev, Communication, Gaming, Home automation…) or by compose project. Containers of the same project share one "stack" card.
- **Orthogonal cables** between linked elements: hovering a network, port, route or disk lights up the linked containers and their cables. "Show all cables" is in the View menu.
- Clicking a container shows its details (networks, ports, mounts, domains) with shortcuts to Docker, the Browser and Files.
- Click-drag to pan, Ctrl + wheel to zoom, filter, editable title.
- Read-only: nothing is changed on the server.

### Fixes
- Containers with *bind* mounts (Jellyfin, for example) were not detected.

### CI
- The Linux build retries apt downloads and gives up after 15 minutes when the Ubuntu mirrors are down.

## [0.4.0] - 2026-11-11

### New

#### 👥 Docker: current visitors of web services
New **Visitors** column in the Docker app: the number of distinct IP addresses connected to each container that serves the web.
- **Direct connections**: established connections on its web ports, read from `/proc/<pid>/net/tcp` without sudo. Connections from other containers or from the Docker host are ignored.
- **Behind Nginx Proxy Manager**: each proxy host is matched to its container (name, service, alias, IP or published port), and the client IPs of the last 5 minutes are read from its access logs. Only the IPs leave the server.
- The tooltip details both sources, the web ports and the sites.
- Read-only.

## [0.3.0] - 2026-11-09

### New

#### 🗄️ Databases: phpMyAdmin-like explorers
- **MySQL / MariaDB and PostgreSQL**: databases, schemas, tables (views are marked), paginated rows with sort and search (all columns or one), table structure.
- **Edit, insert, delete** rows: every write is confirmed with the exact command, only changed columns are sent, and rows are matched by primary key (or by all values, `LIMIT 1`).
- **MongoDB** (through `mongosh`): collections, paginated documents with a text or JSON filter, sort, indexes, replace / insert / delete in extended JSON.
- Values with quotes or control characters are hex-encoded, so they cannot escape the query.
- Credentials are still read inside the container from its Docker config; sshOs never shows or stores them.

## [0.2.0] - 2026-11-09

### New

#### 🧰 New admin apps
- **Services** (systemd): start, stop, restart, enable at boot, status and journal.
- **Updates**: apt / dnf / yum packages, security updates, reboot required.
- **Certificates**: TLS certificates and their expiry date, with a notification 15 days before expiry.
- **Scheduled tasks**: crontab, `/etc/cron.d` and systemd timers.
- **Users and keys**: accounts, groups, administrators, `authorized_keys`, last logins.
- **Network**: interfaces, routes, DNS, listening ports, connections, tools (ping, traceroute, dig, curl).
- **Tunnels**: local SSH port forwards.
- **Backups**: `tar.gz` archive of a folder, restic / borg repositories.
- **Databases**: PostgreSQL, MySQL/MariaDB, Redis and MongoDB containers detected through Docker. MySQL/MariaDB logins use the credentials from the Docker config (`MYSQL_*` / `MARIADB_*`, `*_PASSWORD_FILE` secrets); the password is never shown.
- **Multi-server**: one card per server (load, memory, disk, failed services, updates).
- **Scripts**: reusable commands, run on one or several servers.
- **Security**: new **Firewall** tab (ufw rules, open or block a port with a command preview, SSH port safeguard).

Actions that need `sudo` always go through a Terminal with the command pre-typed.

### Fixes
- Crash when an async action finished after its window was closed.
- The title no longer overlaps the window buttons in apps without a sidebar.

### Other
- German and Spanish translations completed.
- Installers are only built when a version is released.

## [0.1.0] - 2026-11-08

### First release

sshOs is an **agentless** Linux server management desktop: everything goes through SSH and SFTP. It is a Rust desktop app (Tauri v2 + russh, Leptos UI compiled to WebAssembly).

#### Apps
- **Connections**: SSH hosts (password, key, agent), ProxyJump, `~/.ssh/config` import, secrets in the OS keychain, fingerprint checks.
- **Terminal**: remote PTY in xterm.js.
- **Files**: Finder-like SFTP browser, upload and download with drag and drop.
- **Editor** and **Text**: CodeMirror 6, atomic writes, `.bak` backup under `/etc`.
- **Monitor**: CPU, memory, disk, network, GPU, processes.
- **Disks**: partitions, space analysis, guided cleanup.
- **Logs**: `journalctl` and `/var/log`, with live follow.
- **Git**: repositories on the server, status, history, branches, diff.
- **Transfers** and **Command history**: upload queue, and every command run by sshOs with its exit code.
- **Nginx**: native nginx sites and Nginx Proxy Manager in one list.
- **Docker**: containers grouped by compose project, logs, built-in shell, images and volumes.
- **Browser**: the server's web pages through an SSH tunnel.
- **Security**: SSH login attempts, sessions, fail2ban and its visual editor.

#### Principles
- No `sudo` command runs silently: it is pre-typed in a Terminal.
- Every write shows the exact command before it runs.
- `nginx -t` and `fail2ban-client -t` run before every reload.
