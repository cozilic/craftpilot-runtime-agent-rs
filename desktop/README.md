# CraftPilot Agent Setup (Windows)

En liten lokal panel för att installera och övervaka Runtime Agent på Windows. Panelens
webbgränssnitt startas av en enda EXE och öppnas i din vanliga webbläsare.

## Hämta och kör

Ladda ner `CraftPilot.AgentSetup.exe` från
[senaste release](https://github.com/cozilic/craftpilot-runtime-agent-rs/releases/latest/download/CraftPilot.AgentSetup.exe).
Om ingen release har publicerats ännu kan du hämta artifacten **CraftPilotAgentSetup-win-x64**
från **Actions → Desktop setup** i stället.

Kör sedan `CraftPilot.AgentSetup.exe`.

EXE-filen är **osignierad**. Windows SmartScreen kan därför visa en varning första gången:
välj **Mer information → Kör ändå**. Bygg den gärna själv från källkoden om du föredrar det.

## Krav

- Windows 10 eller 11, x64.
- Docker Desktop med **Linux-containrar**.
- **Settings → Advanced → Allow the default Docker socket to be used** är aktiverat.
- Utgående HTTPS till din master, till exempel `https://craftpilot.se`.

Om Docker Desktop saknas visar panelen en varningsruta med nedladdningslänk och inaktiverar
installationen. Om Docker Desktop finns men motorn inte är igång visas i stället en uppmaning att
starta Docker Desktop. Inga compose-filer skrivs förrän Docker är redo.

## Använd panelen

1. Ange masterns URL. Den måste vara en HTTPS-origin utan sökväg.
2. Välj mappen för serverdata. Standard är `%USERPROFILE%\CraftPilot\servers`.
3. Klicka **Installera / starta**. Panelen skapar nätverket, hämtar agentimagen och startar
   containern.
4. När claim-koden visas: kopiera den och öppna **Min nod** via panelen. Klistra in koden där.
5. Statuskorten ska bli gröna för Docker, master och agent. Efter slutförd anslutning visar
   panelen **Redan ansluten** i stället för en ny kod.

**Uppdatera** hämtar den senaste `main`-imagen och startar om agenten. **Stoppa** stoppar
containern men behåller både serverdata och agentens state. Använd `docker compose ... down -v`
manuellt om du vill radera anslutningen och tvinga en ny claim.

## Var sakerna hamnar

| Sak | Plats |
| --- | --- |
| Compose-fil och `.env.desktop` | `%LOCALAPPDATA%\CraftPilot\Agent` |
| Serverdata (standard) | `%USERPROFILE%\CraftPilot\servers` |
| Agentens state | Docker-volymen `craftpilot_rs_agent_state` |
| Agentcontainer | `craftpilot-runtime-agent-rs` |

Compose-filen som används är repots `compose.desktop.yaml`, inbäddad i EXE-filen.

## Säkerhet

Panelens HTTP-server lyssnar bara på `127.0.0.1` och kräver en slumpad token som genereras vid
varje start. Den har ingen inloggning och ska bara köras på din egen dator. Agenten får åtkomst
till Docker-socketen, vilket i praktiken är root på värden – behandla den som en privilegierad
tjänst.

EXE-filen är inte kodsignerad och panelen kontrollerar inte uppdateringar av sig själv. Ladda bara
ner den från repots GitHub Actions eller bygg den själv.

## Bygg och testa

Kräver .NET SDK 8.

```sh
dotnet test desktop/CraftPilot.AgentSetup.Tests/CraftPilot.AgentSetup.Tests.csproj
dotnet publish desktop/CraftPilot.AgentSetup/CraftPilot.AgentSetup.csproj \
  -c Release -r win-x64 --self-contained true \
  -p:PublishSingleFile=true -p:EnableCompressionInSingleFile=true \
  -o desktop/dist/win-x64
```

`desktop.yml` kör testerna och publicerar EXE-filen som artifact på varje ändring under
`desktop/`.

## Om Docker inte fungerar

**Docker Desktop saknas**

Panelen visar **Docker Desktop saknas** och knappen för installation är inaktiverad. Hämta Docker
Desktop via länken i panelen, installera med Linux-containrar och klicka på **Kontrollera igen**.

**Docker Desktop är installerat men inte igång**

Panelen visar **Docker Desktop är inte igång**. Starta Docker Desktop och vänta tills motorn är
redo; panelen hittar den automatiskt inom några sekunder. Installationen är blockerad tills dess.

**Docker-kommandot hittades inte trots att Docker Desktop finns**

Stäng panelen, starta om Docker Desktop och öppna panelen igen. Kontrollera också att
`docker version` fungerar i en ny terminal.

## Begränsningar

- Bara Windows x64. macOS/Linux har `setup.sh` och `compose.desktop.yaml`.
- Panelen visar masterns hälsokontroll, inte inloggad nodstatus från panelen. Öppna **Min nod**
  för den fullständiga vyn.
- Claim-koden läses från agentloggen. Om agenten redan är ansluten visas ingen ny kod.
