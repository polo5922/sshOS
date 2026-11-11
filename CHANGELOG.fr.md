# Journal des modifications

*[English](CHANGELOG.md)*

Toutes les évolutions notables de sshOs. Les versions suivent [SemVer](https://semver.org/lang/fr/).

## [0.5.0] - 2026-11-11

### Nouveautés

#### 🗺️ Nouvelle app Homelab : le schéma interactif du serveur
Le serveur dessiné comme une carte de homelab, construite à partir des données réelles et mise à jour en direct :
- **Matériel** : CPU, mémoire, système, adresses IP, version de Docker.
- **Disques** et systèmes de fichiers où Docker stocke ses données, avec l'espace utilisé et les conteneurs qui s'en servent.
- **Réseaux Docker**, **ports ouverts** sur le réseau et **routes du proxy** (nginx et Nginx Proxy Manager).
- **Services** rangés en colonnes par catégorie (Proxy et sécurité, Applications web, Bases de données, Médias, Productivité, Surveillance, Dev, Communication, Jeux, Domotique…) ou par projet compose. Les conteneurs d'un même projet sont réunis dans une carte « stack ».
- **Câbles** orthogonaux entre les éléments liés : survoler un réseau, un port, une route ou un disque allume les conteneurs reliés et leurs câbles. L'option « Afficher tous les câbles » est dans le menu Présentation.
- Clic sur un conteneur : détails (réseaux, ports, montages, domaines) et raccourcis vers Docker, le Navigateur et Fichiers.
- Déplacement de la vue au clic-glissé, zoom (Ctrl + molette), filtre et titre modifiable.
- Lecture seule : rien n'est modifié sur le serveur.

### Corrections
- Les conteneurs avec des montages de type *bind* (par exemple Jellyfin) n'étaient pas détectés.

### CI
- Le build Linux réessaie les téléchargements apt et s'arrête au bout de 15 minutes en cas de panne des miroirs Ubuntu.

## [0.4.0] - 2026-11-11

### Nouveautés

#### 👥 Docker : visiteurs actuels des services web
Nouvelle colonne **Visiteurs** dans l'app Docker : le nombre d'adresses IP distinctes connectées à chaque conteneur qui sert du web.
- **Connexions directes** : connexions établies sur ses ports web, lues dans `/proc/<pid>/net/tcp` sans sudo. Les connexions venant d'autres conteneurs ou de l'hôte Docker sont ignorées.
- **Derrière Nginx Proxy Manager** : chaque proxy host est associé à son conteneur (nom, service, alias, IP ou port publié), et les IP clientes des 5 dernières minutes sont lues dans ses journaux d'accès. Seules les IP sortent du serveur.
- L'infobulle détaille les deux sources, les ports web et les sites.
- Lecture seule.

## [0.3.0] - 2026-11-09

### Nouveautés

#### 🗄️ Bases de données : explorateurs façon phpMyAdmin
- **MySQL / MariaDB et PostgreSQL** : bases, schémas, tables (les vues sont signalées), données paginées avec tri et recherche (toutes les colonnes ou une seule), structure des tables.
- **Modifier, insérer, supprimer** des lignes : chaque écriture est confirmée avec la commande exacte, seules les colonnes modifiées sont envoyées, et les lignes sont ciblées par clé primaire (ou par toutes les valeurs, `LIMIT 1`).
- **MongoDB** (via `mongosh`) : collections, documents paginés avec filtre texte ou JSON, tri, index, remplacement / insertion / suppression en JSON étendu.
- Les valeurs contenant des guillemets ou des caractères de contrôle sont encodées en hexadécimal : elles ne peuvent pas sortir de la requête.
- Les identifiants sont toujours lus dans le conteneur, depuis sa configuration Docker ; sshOs ne les affiche ni ne les stocke.

## [0.2.0] - 2026-11-09

### Nouveautés

#### 🧰 Nouvelles apps d'administration
- **Services** (systemd) : démarrer, arrêter, redémarrer, activer au démarrage, statut et journal.
- **Mises à jour** : paquets apt / dnf / yum, mises à jour de sécurité, redémarrage requis.
- **Certificats** : certificats TLS et date d'expiration, avec une notification 15 jours avant l'expiration.
- **Tâches planifiées** : crontab, `/etc/cron.d` et timers systemd.
- **Utilisateurs et clés** : comptes, groupes, administrateurs, `authorized_keys`, dernières connexions.
- **Réseau** : interfaces, routes, DNS, ports en écoute, connexions, outils (ping, traceroute, dig, curl).
- **Tunnels** : redirections de ports SSH locales.
- **Sauvegardes** : archive `tar.gz` d'un dossier, dépôts restic / borg.
- **Bases de données** : conteneurs PostgreSQL, MySQL/MariaDB, Redis et MongoDB détectés via Docker. La connexion MySQL/MariaDB utilise les identifiants de la configuration Docker (`MYSQL_*` / `MARIADB_*`, secrets `*_PASSWORD_FILE`) ; le mot de passe n'est jamais affiché.
- **Multi-serveurs** : une carte par serveur (charge, mémoire, disque, services en échec, mises à jour).
- **Scripts** : commandes réutilisables, exécutées sur un ou plusieurs serveurs.
- **Sécurité** : nouvel onglet **Pare-feu** (règles ufw, ouvrir ou bloquer un port avec aperçu de la commande, garde-fou pour le port SSH).

Les actions qui demandent `sudo` passent toujours par un Terminal où la commande est pré-tapée.

### Corrections
- Plantage quand une action asynchrone se terminait après la fermeture de sa fenêtre.
- Le titre ne chevauche plus les boutons de fenêtre dans les apps sans barre latérale.

### Divers
- Traductions allemande et espagnole complétées.
- Les installeurs ne sont plus construits qu'à la publication d'une version.

## [0.1.0] - 2026-11-08

### Première version

sshOs est un bureau de gestion de serveurs Linux **sans agent** : tout passe par SSH et SFTP. C'est une application desktop en Rust (Tauri v2 + russh, interface Leptos en WebAssembly).

#### Apps
- **Connexions** : hôtes SSH (mot de passe, clé, agent), ProxyJump, import de `~/.ssh/config`, secrets dans le trousseau de l'OS, vérification des empreintes.
- **Terminal** : PTY distant dans xterm.js.
- **Fichiers** : navigateur SFTP façon Finder, envoi et téléchargement par glisser-déposer.
- **Éditeur** et **Texte** : CodeMirror 6, écriture atomique, sauvegarde `.bak` sous `/etc`.
- **Moniteur** : CPU, mémoire, disque, réseau, GPU, processus.
- **Disques** : partitions, analyse de l'espace, nettoyage guidé.
- **Journaux** : `journalctl` et `/var/log`, avec suivi en direct.
- **Git** : dépôts du serveur, statut, historique, branches, diff.
- **Transferts** et **Historique des commandes** : file des envois, et toutes les commandes lancées par sshOs avec leur code de retour.
- **Nginx** : sites nginx natifs et Nginx Proxy Manager dans une seule liste.
- **Docker** : conteneurs groupés par projet compose, logs, shell intégré, images et volumes.
- **Navigateur** : pages web du serveur à travers un tunnel SSH.
- **Sécurité** : tentatives de connexion SSH, sessions, fail2ban et son éditeur visuel.

#### Principes
- Aucune commande `sudo` n'est lancée en silence : elle est pré-tapée dans un Terminal.
- Chaque écriture affiche la commande exacte avant d'être exécutée.
- `nginx -t` et `fail2ban-client -t` sont lancés avant chaque rechargement.
