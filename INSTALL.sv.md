# Installera CraftPilot Runtime Agent

Det finns två officiella installationsvägar. Båda kopplar en maskin till CraftPilot och kräver
Docker med **Linux-containrar**.

| Plattform | Rekommenderas för | Verktyg | Start |
| --- | --- | --- | --- |
| Linux x86_64 | 24/7-drift och servrar | Docker Engine + Compose | `setup.sh` |
| Windows 10/11 x64 | Enkel lokal installation | Docker Desktop | `CraftPilot.AgentSetup.exe` |

## Gemensamt innan du börjar

- En master-URL, normalt `https://craftpilot.se`.
- Utgående HTTPS från maskinen till mastern.
- En katalog för serverdata med plats för världar och modpack.
- Docker 27 eller senare. På Windows krävs Docker Desktop med Linux-containrar och
  **Allow the default Docker socket to be used**.
- En claim-kod gäller i 30 minuter och kopplas under **Min nod** i panelen.

Agenten får åtkomst till Docker-socketen och är därför en privilegierad tjänst. Kör den på en
dedikerad maskin eller nod och exponera aldrig Docker-socketen över nätverket.

## Linux

### Snabbstart från senaste release

```sh
curl -fsSLO https://github.com/cozilic/craftpilot-runtime-agent-rs/releases/latest/download/craftpilot-agent-linux.tar.gz
tar xzf craftpilot-agent-linux.tar.gz
cd craftpilot-agent-linux
sudo ./setup.sh
```

`setup.sh` skapar `/opt/craftpilot-agent`, serverkatalogen, Docker-nätverket och `.env`, hämtar
imagen, startar agenten och skriver ut claim-koden. Kör samma kommando igen för att uppdatera.

### Köra från main i stället

Om ingen release har publicerats ännu kan du hämta de två filerna direkt från `main`:

```sh
mkdir -p craftpilot-agent-linux
cd craftpilot-agent-linux
curl -fsSLO https://raw.githubusercontent.com/cozilic/craftpilot-runtime-agent-rs/main/setup.sh
curl -fsSLO https://raw.githubusercontent.com/cozilic/craftpilot-runtime-agent-rs/main/compose.yaml
chmod +x setup.sh
sudo ./setup.sh
```

Fullständig Linux-guide: [GUIDE.sv.md](GUIDE.sv.md).

## Windows

1. Ladda ner `CraftPilot.AgentSetup.exe` från
   [senaste release](https://github.com/cozilic/craftpilot-runtime-agent-rs/releases/latest/download/CraftPilot.AgentSetup.exe).
2. Kontrollera att Docker Desktop är igång med Linux-containrar och att den publika
   Docker-socketen är aktiverad.
3. Kör EXE:n. Panelen öppnas i webbläsaren.
4. Ange master-URL och serverdata, klicka **Installera / starta** och kopiera claim-koden.

EXE:n är osignierad, så Windows SmartScreen kan varna första gången. Välj **Mer information →
Kör ändå**, eller bygg den själv från källkoden.

Fullständig Windows-guide: [desktop/README.md](desktop/README.md).

## Efter installationen

1. Öppna mastern och gå till **Min nod**.
2. Klistra in claim-koden och koppla noden.
3. Kontrollera att agenten körs:

```sh
# Linux
cd /opt/craftpilot-agent
sudo docker compose ps
```

På Windows visar panelen Docker-, master- och agentstatus samt claim-koden.

## Verifiera nedladdningar

Varje release innehåller `SHA256SUMS`:

```sh
sha256sum -c SHA256SUMS
```

På Windows kan du använda `Get-FileHash` eller `certutil -hashfile`.

## Om något går fel

- **Docker saknas:** installera Docker Engine (Linux) eller Docker Desktop (Windows).
- **Docker är stoppat:** starta Docker-tjänsten eller Docker Desktop.
- **Ingen claim-kod:** agenten kan redan vara ansluten. Kontrollera loggen eller panelen.
- **Noden visas offline:** kontrollera att agenten kör, att mastern kan nås och att klockan är rätt.

Vanliga fel och mer felsökning finns i [GUIDE.sv.md](GUIDE.sv.md) och
[desktop/README.md](desktop/README.md).
