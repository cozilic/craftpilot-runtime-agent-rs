# CraftPilot Runtime Agent — snabbguide

Den här guiden startar en Runtime Agent på en Linux-server och kopplar den till CraftPilot.
Du behöver inte bygga något själv.

## Innan du börjar

- Linux **x86_64/amd64**. Den publicerade agentimagen är byggd för amd64.
- **Docker 27 eller senare** med Compose-plugin.
- Utgående **HTTPS** till `https://craftpilot.se`.
- En katalog för serverdata, till exempel `/srv/craftpilot/servers`, med plats för världar och modpack.
- Brandväggen måste tillåta de Minecraft-portar som CraftPilot tilldelar servrarna.
- Ingen GHCR-inloggning behövs när agentimagen är publik. Om du testar innan paketet har
  publicerats behöver du en classic token med `read:packages`; se längst ner.

Kör kommandona med `sudo` om din användare inte är med i `docker`-gruppen.

## Docker Desktop på Mac och Windows

`setup.sh` är Linux-only. Använd `compose.desktop.yaml` i stället. Det här är avsett för
utveckling och test, inte för en 24/7-driftnod.

### Windows-panel (EXE)

Det finns en separat Windows-panel som gör stegen 2–5 åt dig och visar status för Docker,
master och agent. Ladda ner `CraftPilot.AgentSetup.exe` från **Actions → Desktop setup** och
följ [desktop/README.md](desktop/README.md). Panelen använder samma `compose.desktop.yaml` och
körs bara lokalt på din dator.

### 1. Förbered Docker Desktop

- Installera Docker Desktop med Linux-containrar.
- Öppna **Settings → Advanced** och aktivera **Allow the default Docker socket to be used**.
- Starta om Docker Desktop.
- Kontrollera att socketen finns i en Linux-container:

```sh
docker run --rm --entrypoint sh -v /var/run/docker.sock:/var/run/docker.sock docker:27-cli -c 'docker info'
```

Om kommandot inte får kontakt med Docker är inställningen inte aktiv. Använd inte TCP-porten
2375 som genväg; den är oautentiserad och ger i praktiken root på värden.

Apple Silicon: den publicerade agentimagen är amd64. Aktivera Rosetta-emulering i Docker Desktop
eller bygg en lokal arm64-image. För en riktig nod är en Linux x86_64-värd enklare.

### 2. Skapa data och nätverk

Windows PowerShell:

```powershell
New-Item -ItemType Directory -Force C:\craftpilot\servers
docker network create craftpilot-servers
```

macOS:

```sh
mkdir -p ~/craftpilot/servers
docker network create craftpilot-servers
```

### 3. Skapa `.env.desktop`

Windows:

```dotenv
CRAFTPILOT_HOST_DATA_DIR=C:/craftpilot/servers
CRAFTPILOT_SERVER_DATA_ROOT=/srv/craftpilot/servers
CRAFTPILOT_DOCKER_SERVER_DATA_ROOT=C:/craftpilot/servers
CRAFTPILOT_MASTER_URL=https://craftpilot.se
CRAFTPILOT_POLL_SECONDS=5
RUST_LOG=info
```

macOS:

```dotenv
CRAFTPILOT_HOST_DATA_DIR=/Users/DITT_NAMN/craftpilot/servers
CRAFTPILOT_SERVER_DATA_ROOT=/srv/craftpilot/servers
CRAFTPILOT_DOCKER_SERVER_DATA_ROOT=/Users/DITT_NAMN/craftpilot/servers
CRAFTPILOT_MASTER_URL=https://craftpilot.se
CRAFTPILOT_POLL_SECONDS=5
RUST_LOG=info
```

`CRAFTPILOT_HOST_DATA_DIR` är den host-sökväg Docker Desktop delar. `CRAFTPILOT_DOCKER_SERVER_DATA_ROOT`
är den sökväg agenten skickar till Docker när den startar spelservrar.

### 4. Starta agenten

```sh
docker compose --env-file .env.desktop -f compose.desktop.yaml up -d
docker compose --env-file .env.desktop -f compose.desktop.yaml logs -f agent
```

Leta efter claim-koden i loggen och koppla noden under **Min nod**, samma sätt som i Linux-flödet.

### 5. Uppdatera eller stoppa

```sh
docker compose --env-file .env.desktop -f compose.desktop.yaml pull
docker compose --env-file .env.desktop -f compose.desktop.yaml up -d
```

```sh
docker compose --env-file .env.desktop -f compose.desktop.yaml down
```

## 1. Lägg installationsfilerna på servern

Kopiera `setup.sh` och `compose.yaml` till samma mapp på servern, till exempel
`~/craftpilot-agent/`. Om du har en checkout av repot kan du använda den direkt:

```sh
cd craftpilot-runtime-agent-rust
chmod +x setup.sh
```

## 2. Kör installationen

```sh
sudo ./setup.sh
```

Skriptet är idempotent och gör följande:

- skapar `/opt/craftpilot-agent` och `/srv/craftpilot/servers`
- skapar Docker-nätverket `craftpilot-servers`
- skapar `/opt/craftpilot-agent/.env` om den inte finns
- installerar `compose.yaml`
- hämtar agentimagen
- startar agenten
- väntar på och skriver ut claim-koden

Om agenten redan är kopplad skrivs ingen ny kod. Då kan du kontrollera status i panelen.

## 3. Koppla noden i panelen

1. Logga in på `https://craftpilot.se`.
2. Öppna **Min nod**.
3. Klistra in koden från terminalen i fältet **Kod från agenten**.
4. Klicka **Koppla nod**.

Koden gäller i **30 minuter**. Starta om agenten om du behöver en ny kod.

Om du är administratör och noden ska tillhöra ett annat konto kan du i stället godkänna den
under **Drift & logg → Agentförfrågningar**.

Be administratören kontrollera nodens **publika adress** under **Drift & logg → Runtime-noder**.
Utan den kan spelare inte nå servrar som placeras på noden.

## 4. Kontrollera att det fungerar

```sh
cd /opt/craftpilot-agent
sudo docker compose ps
sudo docker compose logs --tail 30 agent
```

Därefter:

1. Skapa en server i panelen.
2. Välj noden som värd/host.
3. Starta servern.
4. Kontrollera att containern körs:

```sh
sudo docker ps --filter name=craftpilot-
```

## Uppdatera agenten

```sh
cd /opt/craftpilot-agent
sudo docker compose pull
sudo docker compose up -d
```

State-volymen behålls, så noden behöver inte kopplas om.

`main` är en rörlig tagg. I produktion bör ni låsa en specifik version eller digest i
`compose.yaml`, till exempel `ghcr.io/cozilic/craftpilot-runtime-agent-rs:sha-8a2f263`, när en
sådan har verifierats.

## Avinstallera

Behåll nodens credential:

```sh
cd /opt/craftpilot-agent
sudo docker compose down
```

Ta bort även state och tvinga en ny koppling:

```sh
sudo docker compose down -v
```

`down -v` raderar credentialen och RCON-lösenorden. Använd bara det om du är säker.

## Om något går fel

**`unauthorized` när imagen hämtas**

Paketet är ännu inte publikt. Logga in med en classic token som har `read:packages`, eller be
administratören publicera GHCR-paketet.

**`network craftpilot-servers not found`**

```sh
sudo docker network create craftpilot-servers
```

**`permission denied` mot `/var/run/docker.sock`**

Kör setupen som en användare med Docker-åtkomst, eller använd `sudo`.

**Ingen claim-kod i loggen**

```sh
cd /opt/craftpilot-agent
sudo docker compose logs agent | grep -i claim
```

Om agenten redan har en sparad credential skrivs ingen ny kod. Koden skapas bara när noden
behöver kopplas eller kopplas om.

**Noden visas offline**

Kontrollera att agenten kör, att den når `https://craftpilot.se` och att serverns klocka är rätt.

**Servern startar men spelare kan inte ansluta**

Kontrollera brandväggen, den publika adressen i panelen och att rätt Minecraft-port är öppen.

**`CRAFTPILOT_MASTER_URL must use https`**

Använd exakt en HTTPS-origin utan path, till exempel `https://craftpilot.se`.

## För maintainers: medan paketet är privat

Så länge GHCR-paketet inte är publikt måste noden logga in innan `setup.sh` kan hämta imagen:

```sh
sudo docker login ghcr.io -u DITT_GITHUB_ANVÄNDARNAMN
```

Använd en **classic personal access token** med `read:packages`. Använd inte GitHub-lösenordet
och inte en fine-grained token. När paketet är publikt kan det här steget tas bort.

## Säkerhet

Agenten har åtkomst till Docker-socketen och är därför en privilegierad tjänst. Publicera aldrig
Docker-socketen eller agentens state-volym. Kör agenten på en dedikerad server, skydda
`/var/lib/craftpilot-agent` och lägg aldrig tokens eller credentials i git eller i chatten.
