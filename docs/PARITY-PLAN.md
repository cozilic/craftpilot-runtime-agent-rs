# Rust-agenten: vad som återstår innan den ersätter Python-agenten

**Status: förslag (2026-11-25).** Gäller detta repo; ingen kod ingår. Planen utgår
från masterns kontrakt i `docs/RUNTIME-NODE-SFTP.md` och `docs/API.md` i
[CraftPilot](https://github.com/cozilic/CraftPilot).

## Utgångsläge

Rust-agenten (v0.12.4, 2026-11-24) har full kommandoparitet — alla sjutton
typer i `RuntimeNodeCommandType` hanteras — och är den enda agenten som
rapporterar `sftp_bound_port`, så masterns portval och Ops-varning fungerar
mot den. Det som skiljer den från Python-agenten är tre saker, varav en är
funktionell och två är beslut.

| # | Fråga | Blockar ersättning? |
| --- | --- | --- |
| 1 | `sftp-password`-kanalen saknas (`features: ["sftp"]` bara) | Ja, för noder med lösenordsinloggning |
| 2 | Endast Ed25519-värdnyckel | Beslut + dokumentation |
| 3 | SFTPGo `v2.6.6` mot masterns `v2.6.4` | Nej, men bör alignas |

## 1. `sftp-password` mot masterns secrets/status-endpoints

Mastern är klar sedan 2026-11-21 (PR 1–2): revision, hemlighetstabell,
endpoints, TTL, kvittens och feature-flagga. Agentdelen är PR 3 och saknas i
Rust-agenten.

### Vad agenten måste göra

1. **Annonsera förmågan.** `features` ska innehålla `sftp-password` **endast**
   när SFTPGo svarar — samma villkor som i dag gäller för `sftp` och
   värdnyckeln. En nod som tappar SFTPGo ska sluta lova båda.
2. **Läsa revisionen i det vanliga state-svaret.** `GET /api/runtime/nodes/{node}/sftp`
   bär per användare `password_set` (bool) och `password_revision` (uint, 0 =
   inget lösenord). Revisionen är auktoritativ: agenten jämför med sin lokala
   revision per konto och hämtar bara när masterns är högre.
3. **Hämta hemligheten en gång.** `POST /api/runtime/nodes/{node}/sftp/secrets`
   med tom body ger `{secrets: [{username, password, revision}]}`. Budget
   10/min; svaret är `no-store`. Ett svar för en användare som inte längre
   finns i state, eller för en annan revision, ska ignoreras — aldrig
   appliceras.
4. **Applicera utan att lagra.** Lösenordet får bara finnas i minnet mellan
   secrets-svaret och `PUT /api/v2/users/{username}` mot nodens SFTPGo, och
   ska nollas efter anropet. Det får aldrig hamna i `state.json`, loggar,
   statusrapporten eller claimen. `password` utelämnas när hashen redan är
   applicerad (SFTPGo behåller då hashen); tom sträng rensar.
5. **Kvittera.** `POST /api/runtime/nodes/{node}/sftp/status` med
   `{users: [{username, password_revision}]}` (max 500) → `204`. Mastern
   raderar den väntande hemligheten först när alla målnoder kvitterat
   revisionen. Ett okänt användarnamn ger `422 SFTP_UNKNOWN_USER` — det ska
   behandlas som "hoppa över", inte som ett fel som stoppar hela pollcykeln.
6. **Behåll regeln om autentiseringsmetod.** Ett konto utan publik nyckel
   kräver ett applicerat lösenord; annars tas kontot bort. Det gäller även när
   ett lösenord rensas eller TTL löper ut.

### Följder som ska stå i releaseanteckningen

- Utan kanalen blir `node_password_state` `unsupported` för varje rs-nod, och
  en användare som bara har lösenord får inget konto alls.
- Masterns flagga `SFTP_NODE_PASSWORD_ENABLED` är **av** som default. End-to-end
  går därför inte att verifiera förrän den slås på lokalt/preview, och
  panelens "ange igen"-flöde (masterns PR 4) måste finnas innan flaggan slås på
  i produktion. Agenten kan bli klar före masterns utrullning, men inte
  verifieras före den.

### Tester som hör till PR:en

Hämtning sker en gång per revision · omstart utan klartext i state · TTL-miss
ger keys-only konto · clear tar bort lösenordet · kvittensen driver masterns
radering · `422` för okänt användarnamn stoppar inte cykeln · `features`
innehåller `sftp-password` bara när SFTPGo svarar.

## 2. Beslut: Ed25519-only värdnyckel

Agenten startar SFTPGo med `SFTPGO_SFTPD__HOST_KEYS=id_ed25519`, så RSA/ECDSA
varken serveras eller rapporteras. Det är ett medvetet val — fingeravtrycket i
panelen är då detsamma som varje klient ser — men det har en konsekvens som
måste vara uttalad: **en klient som inte förhandlar Ed25519 kan inte ansluta
till en rs-nod alls**, och panelen visar ett fingeravtryck som klienten aldrig
får se.

Underlaget föreslår att valet behålls, med två villkor:

- Det står i releaseanteckningen och i `GUIDE.sv.md` som en känd begränsning,
  inte bara som en teknisk detalj i README.
- Panelen kan skilja "nodens SFTP svarar" från "klienten förhandlade ingen
  gemensam nyckel". Om ett sådant läge inte går att skilja i dag är det ett
  master-ärende, inte ett agent-ärende.

Alternativet — servera även RSA — gör värdnyckelns fingeravtryck
icke-deterministiskt mellan klienter, vilket är dyrare att förklara i support
än att be en gammal klient byta algoritm. Det är därför inte rekommenderat.

## 3. SFTPGo-versionen avviker

Rust-agenten defaultar till `drakkan/sftpgo:v2.6.6`; masterns egen SFTPGo (och
Python-agenten) kör `v2.6.4`. Samma funktion får därmed olika bild beroende på
nodtyp — och masterns `sftp-probe` kör i SFTPGo-containerns nätverksnamespace,
så en avvikelse slår mot diagnostiken, inte bara mot inloggningen.

Förslag: lås rs-agenten till samma `v2.6.4` som mastern tills en uppgradering
görs gemensamt, och skriv i README varför versionen är pinnad (den är en del av
kontraktet, inte en smaksak). Uppgraderar man först i rs-agenten ska
`docs/RUNTIME-NODE-SFTP.md` i mastern uppdateras i samma veva.

## Ordning

1. SFTPGo-versionen till `v2.6.4` (en rad + README-motivering).
2. Ed25519-beslutet skrivs ned (releaseanteckning + guide).
3. `sftp-password` som egen PR med testlistan ovan.
4. Först därefter ändras masterns `runtime-agent/README.md` och den lokala
   overlayen från `:0.10` till `:0.12`, och "Kvar: agentrapporterad port" i
   `docs/RUNTIME-NODE-SFTP.md` stryks för rs-agentens del.

Steg 1–2 kan göras utan masterändringar. Steg 3 kräver att flaggan kan slås på
i en testmiljö; steg 4 är masterns beslut.
