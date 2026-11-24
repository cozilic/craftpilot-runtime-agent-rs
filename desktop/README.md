# CraftPilot Agent Setup (Windows)

En liten lokal panel för att installera och övervaka Runtime Agent på Windows. Panelens
gränssnitt startas av en enda EXE och visas i ett eget Windows-fönster med WebView2.

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
- Microsoft Edge WebView2 Runtime. Det finns normalt redan på Windows 10/11.
- Docker Desktop med **Linux-containrar**.
- **Settings → Advanced → Allow the default Docker socket to be used** är aktiverat.
- Utgående HTTPS till `https://craftpilot.se`.

Om WebView2 Runtime saknas visas en upplysning och panelen öppnas i den vanliga webbläsaren i
stället. Om Docker Desktop saknas visar panelen en varningsruta med en knapp som hämtar Dockers
officiella installationsprogram till `%TEMP%`, verifierar Authenticode-signaturen och startar det
(UAC och Docker:s egen guide sköter resten), plus en länk till Docker för manuell nedladdning.
Medan filen laddas visar rutan procent och hämtad storlek. **Avbryt hämtningen** stoppar en
pågående nedladdning direkt. Nedladdningen skrivs till
`Docker Desktop Installer.exe.part` och får sitt riktiga namn först när allt är hämtat; en
halvfärdig fil tas bort vid avbrott och innan nästa försök.
Installationen av agenten är inaktiverad tills Docker är redo. Om Docker Desktop finns men motorn
inte är igång visas i stället en uppmaning att starta Docker Desktop. Inga compose-filer skrivs
förrän Docker är redo.

## Använd panelen

1. Panelen ansluter alltid till mastern `https://craftpilot.se`; adressen kan inte ändras.
2. Välj mappen för serverdata. Standard är `%USERPROFILE%\CraftPilot\servers`.
3. Klicka **Installera / starta**. Panelen skapar nätverket, hämtar agentimagen och startar
   containern. Finns redan en container med namnet `craftpilot-runtime-agent-rs` (från en
   tidigare installation eller `runtime-agent/start-rs-agent.ps1`) tar panelen bort den först —
   compose vägrar annars återanvända namnet och installationen fastnar.
4. När claim-koden visas: kopiera den och öppna **Min nod** via panelen. Klistra in koden där.
5. Statuskorten ska bli gröna för Docker, master och agent. Efter slutförd anslutning visar
   panelen **Redan ansluten** i stället för en ny kod.

## Språk (svenska och engelska)

Panelens gränssnitt finns på svenska och engelska. Byt språk under **Inställningar → Språk**;
texterna byts direkt och valet sparas i panelens `localStorage` för nästa start. Första gången
följer panelen Windows språk om det är svenska eller engelska, annars svenska.

Panelens egna texter, knappar och dialogrutor översätts. Meddelanden som agenten eller panelens
C#-del skriver — detaljerade feltexter och själva agentloggen — är kvar på svenska.

## Översiktens statistik och nodnamn

Överst i **Översikt** står nodens namn och datorns resursläge: antal servrar (körda av totalt),
spelare online, RAM, disk och nätverkstrafik. Statistiken hämtas lokalt från Docker, Windows och
serverdata-mappen; mastern behöver inte vara nåbar.

Nodnamnet läses ur agentloggen (`node_name=...`). Det kräver en agentimage som skriver ut namnet
vid start och när panelen byter namn på noden — äldre images visar i stället datorns hostname.
Spelarantalet kommer från Minecrafts server-list-ping mot serverns publicerade port; en server som
inte svarar visas som **—** i stället för noll spelare.

Kortet **SFTP** visar porten SFTPGo-containern faktiskt använder och om tjänsten svarar på den.
Porten läses ur Docker, inte ur `.env.desktop`: agenten tar nästa lediga port när
`CRAFTPILOT_SFTP_PORT` är upptagen, och då står den önskade porten som en rad under kortet.
Samma uppgifter finns i diagnosfilen under `[SFTP]`.

Korten **Agent** och **SFTP** har **Starta**/**Stoppa** direkt i översikten. Agentens **Stoppa**
stänger bara av agentcontainern; SFTPGo-containern och dess sessioner ligger kvar. SFTP:s
**Stoppa** är samma sak som switchen i Inställningar — flaggan skrivs av, sessioner kopplas ner
och containern tas bort medan volymerna (och värdnyckeln) ligger kvar. **Starta** slår på
flaggan igen och agenten startar SFTPGo på samma volymer och portval som tidigare.

SFTPGo startas och stoppas av agenten, så SFTP-knapparna är låsta när agenten är stoppad —
annars hade de startat agenten som en bieffekt av att man bara ville stänga av SFTP. Starta
agenten först; flaggan kan ändå sättas i Inställningar och gäller nästa start.

Under **Inställningar → SFTP på den här noden** slås nodens SFTP av och på. Panelen skriver
`CRAFTPILOT_SFTP_ENABLED` i `.env.desktop` och återskapar agentcontainern; rader panelen inte
äger, t.ex. `CRAFTPILOT_RS_AGENT_IMAGE_TAG`, lämnas orörda. Är flaggan av tar agenten bort
`craftpilot-sftpgo` — volymerna ligger kvar, så värdnyckeln och SFTPGo-databasen består och
nästa påslag återanvänder dem.

Servrar som skapats i panelen men ännu inte startats listas också under **Servrar på den här
noden**, med status **Inte startad**. De har en katalog i serverdata-mappen men ingen container
ännu; **Rensa stoppade** rör dem inte.

Serverraderna är klickbara: de öppnar servern i panelen i din vanliga webbläsare, där start, stopp
och konsol finns.

## Versionskontroll

Panelen frågar GitHub efter en ny panelversion enligt valet i **Inställningar → Sök ny panelversion**:
**En gång om dagen** (standard), **En gång i veckan** eller **Bara manuellt**. Valet och tiden för
den senaste kontrollen sparas i `%LOCALAPPDATA%\CraftPilot\panel-settings.json`, så panelen minns
dem efter en omstart. **Kontrollera nu** frågar GitHub direkt, oavsett intervall — det är också enda
vägen när **Bara manuellt** är valt. Nästa automatiska kontroll räknas från den senaste kontrollen,
inte från att panelen startade.

**Kontrollera nu** visar vad som händer i panelens progress-bar: **Kontrollerar ny version…**, och
sedan **Ny version v0.12.2 hittad** under knappen. Finns en nyare version blir **Ladda ner och
starta om** synlig på samma rad — den laddar ner exen och visar **Hämtar versionsinformation…**,
**Hämtar ny version…** (med procent), **Kontrollerar filen…**, **Installerar panelen…** och
**Startar om panelen…**. Samma knapp finns i bannern i Översikt.

## Tray och automatisk start

Panelen har en tray-ikon som visar om Docker, mastern och agenten är redo. Stäng fönstret med
**X** för att lägga det i trayen i stället för att avsluta panelen. Välj **Öppna panelen** i
tray-menyn för att visa den igen och **Avsluta** för att stoppa panelen.

Bocken **Starta med Windows** lägger en post under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` och startar panelen med `--startup` vid
inloggning. Då visas bara tray-ikonen; panelens fönster skapas först när du öppnar det. Samma
inställning finns i tray-menyn.

Vid panelstart väntar panelen in Docker i upp till två minuter. Saknas agentcontainern startar
panelen compose-projektet själv; en container som finns men är stoppad rörs inte. Har du tryckt
**Stoppa** i panelen kommer agenten inte tillbaka förrän du trycker **Starta** — stoppet sparas i
panelens inställningsfil. Kommer agenten inte upp inom tre minuter från startförsöket visar trayen
en notis i stället för att noden tystnar.

**Avbryt** avbryter en pågående åtgärd (installation, uppdatering, stopp eller borttagning).
**Uppdatera** hämtar den image som compose-filen pekar på (`:0.12`) och startar om agenten. **Stoppa** stoppar
containern men behåller både serverdata och agentens state. **Ta bort** tar bort containern och
panelens installationsfiler (`.env.desktop` och `compose.desktop.yaml`); serverdata, agentens
state-volym och nodens credential ligger kvar, så nästa installation återansluter samma nod.
Använd `docker compose ... down -v` manuellt om du vill radera anslutningen och tvinga en ny
claim.

Rust-agenten kör servrarna som Docker-containrar. Docker Desktop måste därför vara igång; det
finns ingen Docker-fri start i den här panelen.

## Uppdatera panelen

Panelen jämför automatiskt sin egen version med den senaste stabila GitHub-releasen när den
startar. Om en nyare version finns visas en rad med versionsnummer och en nedladdningslänk.
**Kontrollera igen** gör om både status- och versionskontrollen. Kontrollen kräver HTTPS till
`api.github.com` och resultatet cachas i tio minuter; ett nätverksfel påverkar inte Docker- eller
agentstatusen.

## Notiser, snabbåtgärder och felsökning

Tray-ikonen visar Docker-, master- och agentstatus. Panelen visar dessutom en Windows-notis när
Docker eller agenten stannar, när mastern inte svarar, när disken börjar bli full, när en server
kraschar med en exitkod och när en ny panelversion finns. Samma fel notifieras högst en gång per
cooldown, så en långvarig störning inte fyller skärmen.

Tray-menyn har **Starta om agenten** (startar bara om agentcontainern) och **Öppna loggen** (öppnar
panelens loggflik).

Under **Inställningar → Felsökning** skapar **Skapa diagnos** en textfil med versioner, Docker-läge,
serverlistan och agentloggen. Claim-koder, tokens och lösenord redigeras automatiskt. Filen kan
kopieras eller laddas ner och skickas med en supportfråga.

Serverraderna visar CPU och minne för körande containrar och har en **Starta om**-knapp. Panelen
adresserar bara containrar som den hittar via `craftpilot.managed`-labeln och det server-id som
anropet gäller.

## Var sakerna hamnar

| Sak | Plats |
| --- | --- |
| Compose-fil och `.env.desktop` | `%LOCALAPPDATA%\CraftPilot\Agent` |
| WebView2-data | `%LOCALAPPDATA%\CraftPilot\Agent\WebView2` |
| Serverdata (standard) | `%USERPROFILE%\CraftPilot\servers` |
| Agentens state | Docker-volymen `craftpilot_rs_agent_state` |
| Agentcontainer | `craftpilot-runtime-agent-rs` |

Compose-filen som används är repots `compose.desktop.yaml`, inbäddad i EXE-filen.

## Säkerhet

Panelens HTTP-server lyssnar bara på `127.0.0.1` och kräver en slumpad token som genereras vid
varje start. Varje anrop måste dessutom ha rätt `Host` (`127.0.0.1:<port>` eller `localhost:<port>`),
så en domän som pekar om till loopback via DNS-rebinding avvisas – även sidan som bär tokenen. POST,
PUT, PATCH och DELETE från en annan origin, eller med `Sec-Fetch-Site: cross-site`/`same-site`,
får `403 forbidden`; anrop utan webbläsarhuvuden (curl, skript) måste ändå ha tokenen. Fönstret får
bara navigera till panelens egen loopback-adress; externa länkar öppnas i standardwebbläsaren. Panelen har ingen inloggning och ska bara köras på din egen dator. Agenten
får åtkomst till Docker-socketen, vilket i praktiken är root på värden – behandla den som en
privilegierad tjänst.

Panelen hämtar Docker Desktop-installationsprogrammet från `https://desktop.docker.com/` och kör
det bara om Authenticode-signaturen är giltig och certifikatets utgivare är Docker. Nedladdningen
får inte komma från någon annan värd, och filen tas bort när kontrollen fallerar. Panelen
installerar aldrig Docker tyst.

EXE-filen är inte kodsignerad. Panelen jämför sin version med senaste GitHub-releasen och visar
en länk när en nyare version finns. Ladda bara ner den från repots GitHub Releases eller bygg
den själv.

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

## Testa i Windows Sandbox

Windows Sandbox kräver Windows 10/11 Pro, Enterprise eller Education. Sandboxen startar med
Smart App Control på, vilket blockerar osignerade builds.

`desktop/CraftPilot.Sandbox.wsb` stänger av Smart App Control endast i den tillfälliga
sandboxen. Kör den genom att dubbelklicka på filen eller välja **Öppna med → Windows Sandbox**.

1. Bygg eller ladda ner `CraftPilot.AgentSetup.exe`.
2. Starta `desktop/CraftPilot.Sandbox.wsb`.
3. Kopiera EXE:n till sandboxen och kör den.
4. Stäng sandboxen när du är klar; nästa session börjar med Smart App Control på igen.

Kommandot i `.wsb`-filen ändrar bara sandboxens register. Kör det inte på din vanliga
Windows-installation. Vill du undvika att kopiera filen kan du lägga till en `MappedFolder` med
absolut sökväg till `desktop/dist` i `.wsb`-filen.

Om filen fortfarande blockeras kan du kontrollera statusen i sandboxen:

```powershell
Get-MpComputerStatus | Select-Object SmartAppControlState
```

`SmartAppControlState` ska vara `Off`. Starta i så fall om sandboxen och försök igen.

## Om Docker inte fungerar

**Docker Desktop saknas**

Panelen visar **Docker Desktop saknas** och knappen för installation är inaktiverad. **Hämta och
oppna installationsprogrammet** laddar ner Dockers officiella installationsprogram till `%TEMP%`
och startar det först sedan Authenticode-signaturen verifierats med Docker som utgivare. Windows
frågar om behörighet och installationsprogrammet visar Docker:s villkor och WSL2-val. Länken
**Hämta Docker Desktop** finns kvar som fallback om hämtningen eller signaturkontrollen
misslyckas, eller om du hellre installerar själv. Installera med Linux-containrar och klicka på
**Kontrollera igen**.

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
