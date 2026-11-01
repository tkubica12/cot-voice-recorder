# Migrace Windows a cloudove casti do Rustu

## Rozhodnuti a rozsah

Postupna migrace do Rustu, nejdrive Windows a potom cloud. Nejde o jednorazovy
prepis: nejprve overit prinos na funkcnim pilotu, potom prenaset chovani a data.
Android, Azure sluzby a Bicep zustavaji zachovany.

Windows je silnejsi kandidat. Jednorazovy snimek beziciho procesu z 1. listopadu
2026 ukazal priblizne 163 MiB working setu a 201 MiB private bytes. Neni to
reprezentativni idle baseline ani mereni startu. Private bytes nejsou totez co
fyzicka rezidentni pamet.

Soucasne WPF nelze jednoduse odlehcit zapnutim NativeAOT. Microsoft dokumentuje
[nekompatibilitu WPF s trimmingem](https://learn.microsoft.com/en-us/dotnet/core/deploying/trimming/incompatibilities#wpf).
V cloudu ocekavame mensi image, rychlejsi start procesu a nizsi pametovou rezii,
nikoli automaticky rychlejsi AI prepis reci. AI, sit a Azure infrastruktura
zustavaji stejne. API aktualne drzi jednu teplou repliku; worker skaluje na nulu.
Pridelene prostredky API jsou 0.5 vCPU / 1 GiB a workeru 0.25 vCPU / 0.5 GiB;
jde o konfiguraci, nikoli namerenou spotrebu nebo slib nizsi ceny.

Bezpecnost neni automaticky zisk. C# a Python uz maji spravovanou pamet. Rust
pridava silnejsi kontrolu vlastnictvi a nekterych soubeznych pristupu, ale
nenahradi spravnou autentizaci, recovery ani bezpecne zachazeni s Win32.

## Aktivace a Git workflow

Uzivatel 1. listopadu 2026 schvalil ulozeni tohoto planu, zahajeni prace a
postupne commity a pushe do samostatne vetve `refactor-rust`.

- Pracovat v teto vetvi v existujicim checkoutu, ne v nove worktree.
- Commitovat ucelene zmeny po odpovidajicim overeni a pushovat pouze tuto vetev.
- Nemergovat do `main`, nevytvaret release ani nenasazovat do produkce.
- Nemazat nebo neprepisovat osobni historii, tokeny, recovery ani cloudova data.
- Pouzivat lokalni synteticka data a oddeleny testovaci stav.
- Zachovat soucasnou C#/Python implementaci jako referenci a rollback, dokud
  neni konkretni nahrada kompletni a overena.
- Externi Rust zavislosti pridavat teprve po overeni schvaleneho Cargo registry
  a dostupnosti verzi. Prvni zaklad muze pouzivat pouze standardni knihovnu
  a sestavovat se offline bez pristupu k registry.

Schvaleni prace neni schvaleni placenych AI volani, instalace novych cloudovych
sluzeb, zmen opravneni, produkcniho nasazeni nebo oslabeni akceptacnich cilu.

## Poradi migrace

| Etapa | Konkretni vysledek | Podminka pokracovani |
| --- | --- | --- |
| 1. Vychozi mereni | Opakovatelna mereni soucasneho Windows klienta a backendu, katalog chovani a formatu | Dostatecna baseline pro ferove porovnani |
| 2. Windows pilot | Nativni tray, OAuth/DPAPI, PubSub -> clipboard a zakladni audio/streaming cesta | Prokazany prinos v presne vymezenem rozsahu pilotu |
| 3. Kompletni Windows klient | Settings, History, overlay, oba rezimy diktovani, recovery, audit outbox, instalator | Funkcni a datova kompatibilita; znovu zmereny kompletni klient |
| 4. Rust cloud API | HTTP API a WebSocket relay; puvodni Python worker docasne zustava | Stejny kontrakt a kompatibilni ulozena data |
| 5. Worker a cleanup | Leases, replay, legacy pipeline, finalizace, notifikace a uklid | Poruchove scenare a bezpecny prechod bez dvojiteho zpracovani |

Pro Windows je navrzene nativni UI bez WebView, protoze hlavnim motivem je
nizka rezie. Platformni Win32 operace maji zustat izolovane od bezpecne domenove
logiky. Pro backend je navrzene Tokio/Axum. Nejde o schvaleni konkretni verze
zavislosti nebo o tvrzeni, ze jsou jiz dostupne ve schvalenem registry.

Azure ma moderni Rust knihovny pro
[Identity, Blob a Queue](https://learn.microsoft.com/azure/developer/rust/azure-sdk-library-package-index).
Adaptery pro Tables a Web PubSub potrebuji samostatne overeni. PubSub umoznuje
[REST/WebSocket integraci bez SDK](https://learn.microsoft.com/azure/azure-web-pubsub/overview#why-use-azure-web-pubsub).
Pouzivat stale managed identity, privatni Storage a serverem zvolene AI endpointy.
Nerozsirovat pristup Windows klienta primo na Azure kvuli pohodli prepisu.

## Neprekrocitelne podminky kompletni migrace

- Zachovat HTTP tvary, stavove kody a WebSocket protokol z OpenAPI. Soucasne
  pytesty proti FastAPI nebo xUnit proti C# samy o sobe neoveruji Rust.
  Prenest scenare do testu Rustu a externich kontraktnich testu.
- Zachovat Google ID token jako backend bearer, allowlist, PKCE/state,
  obnovu tokenu a soucasnou DPAPI CurrentUser ochranu.
- Overit cteni starych recovery v1-v4, nastaveni, historie a pending auditu
  na syntetickych fixturech. Overit take data vytvorena Rustem proti staremu
  klientovi, pokud ma byt mozny rollback bez migrace dat.
- Provisionalni text pouze zobrazovat. Nikdy nevlozit neuplny text po selhani
  nebo prekroceni stop budgetu. Recovery nikdy automaticky nevklada.
- Zachovat neaktivujici overlay, klavesove zkratky, vyber cile pri startu
  hold-to-talk a pri stopu hands-free, ochranu focusu a clipboard sekvence,
  jedine Ctrl+V bez Enter a viditelny fallback.
- Zachovat checkpointy, absolutni PCM cursory, presne poradi vzorku a reconnect
  replay. ACK paketu neni potvrzeny prepis a nesmi uvolnit durable audio.
  Nepretvaret soucasny checkpointovy model na slib bezeztratoveho archivu.
- Zachovat omezeni front, chyby disku/kvoty, 48hodinovou retenci a 256 MiB
  recovery kvotu. Nesnizovat pamet zahazovanim audia.
- Zachovat durable audit outbox, fixni expiraci od puvodniho dokonceni,
  idempotentni retry a 409 pri konfliktu. Audit nesmi emitovat automaticky
  clipboard event ani blokovat paste.
- Zachovat Blob/Queue/Table formaty, ETag a vlastnictvi leases, jejich obnovu,
  ordered full replay, legacy pipeline a semantiku finalizace/cleanup.
- Pro prechod workeru nepoustet dva alternativni enginy na stejne recording
  bez prokazaneho fencing/lease protokolu. Zmena jazyka neresi distribuovane zavody.
- Zachovat per-user instalaci, jedinou instanci a jediny autostart mechanismus,
  upgrade bez force-kill a absenci OAuth konfigurace/secrets ve verejnem baliku.
- Znovu overit klavesnicovou pristupnost, DPI a ctecku obrazovky v nativnim UI.
  Funkcni pilot bez techto povrchu neni kompletni nahrada.

## Goal Card: mereny Rust pilot

### OBJECTIVE

Card: RUST-PILOT | Version: 1 | Type: experiment | Owner: uzivatel

Status: lokalni prace a Git workflow schvaleny; kompletni pilot zustava
`Draft - not ready to run`, dokud nejsou overeny vsechny jeho prerequisites.
Dependency-free zaklad a vychozi lokalni mereni mohou pokracovat samostatne.

Rozhodnout zvlast pro Windows a backend, zda prinos Rustu ospravedlnuje dokonceni
migrace. Pilot neni produkcni nahrada.

H1: Windows dosahne nize uvedenych cilu pri zachovani pilotnich funkci.
H2: backend dosahne pametovych a startovacich cilu bez zhorseni spravnosti
a odezvy. Namerene nesplneni cile hypotezu vyvraci. Nefunkcni prostredi
znamena blokaci, nikoli vyvraceni.

### OUTPUT

Dve srovnani, Windows a backend, s nezkracenymi vysledky mereni, rozsahem pilotu
a zaverem "pokracovat / nepokracovat". Podklady `rust-pilot-windows.json`
a `rust-pilot-backend.json` v artefaktech relace; zaver v konverzaci.
Tento soubor udrzovat jako plan a strucny stav migrace. Neuplne podklady oznacit
jako neuplne; nenahrazovat chybejici mereni odhadem.

### DONE WHEN

Cisla jsou navrzene pilotni cile, ne namerene vysledky ani zaruky.

| Metrika | Windows | Backend na Linuxu |
| --- | --- | --- |
| Pamet v idle | Private bytes <=80 MiB a <=50 % baseline | RSS <=50 % baseline |
| Start procesu, p95 | <=1 s a <=70 % baseline | Proces -> readiness <=1 s a <=50 % baseline |
| Odezva | Lokalni pripravenost mikrofonu nezhorsena o vice nez 10 % | p95 vybranych API operaci nejvyse 110 % baseline |
| Stabilita | Po hodinove nahravce a navratu do stejneho idle stavu narust private bytes <=10 MiB | Zadne nove chyby pri stejne zatezi |

1. **C01 - Ferova baseline.** Stejny stroj, release build, vstupy, historie
   a konfigurace; 30 startu a tri serie zateze. Overit ulozene vysledky.
   Pri rozdilnych podminkach mereni zopakovat a zneplatnit C02.
2. **C02 - Verdikt H1/H2.** Vyhodnotit vsechny predepsane serie, ne nejlepsi
   beh. Dukazem jsou podklady a vypocet metrik. Chybejici mereni doplnit;
   prekroceny cil priznat, nesnizovat automaticky prah. Po zmene znovu C02.
3. **C03 - Spravnost pilotu.** Porovnat stejne scenare proti soucasne
   implementaci, vcetne odmitnuti neplatne autentizace, reconnectu
   a opakovanych pozadavku. Dukazem jsou scenare a jejich vystupy.
   Opravit odchylky a zopakovat C03 i dotcena mereni C01/C02.
4. **C04 - Rozhodnuti a uklid.** Zaver odpovida dukazum, zadne pomocne
   procesy nezustaly spustene a produkce nebyla zmenena. Overit skutecny
   stav pred ukoncenim; pri selhani dokoncit povoleny uklid a znovu C04.

U kazdeho checku zaznamenat `NOT_RUN`, `PASS`, `FAIL` nebo `BLOCKED` a dukaz.
Zadny check neprosel pouze tim, ze existuje plan nebo Rust projekt.

### QUALITY

Mensi neuplny pilot nesmi byt vydavan za dukaz spotreby kompletni nahrady.
Lokalni benchmark s fake AI neni mereni skutecne cloudove latence ani ceny.
Bezpecnost a spravnost maji prednost pred usporou prostredku.

Mereni startu procesu nesmi byt zameneno s casem prvni autentizovane cloudove
transkripce. CPU merit jako rozdil procesoroveho casu za skutecny interval,
oddeleny od celkoveho casu CPU od startu. Rozlisit procento jednoho logickeho
jadra a procento celeho stroje. U vzorku 30 startu je p95 pilotni odhad,
nikoli produkcni SLA.

### CONTEXT

Autoritativni podklady: `openapi\voice-recorder.yaml`,
`docs\windows-dictation.md`, `docs\mobile-streaming.md`,
`docs\architecture.md` a soucasne testy.

Preflight musi overit Rust/MSVC, Linuxove merici prostredi, schvaleny Cargo
registry pro externi zavislosti a minimalni lokalni pruchod.
Soucasne testy jsou katalog ocekavaneho chovani; jejich pruchod proti C#/FastAPI
neoveruje Rust. Schvaleny zdroj externich Rust zavislosti zustava otevreny.

Referencni prikazy z komponent:

```powershell
# Z windows:
dotnet test VoicePrompt.sln -c Release

# Z backend:
uv run pytest --ignore=tests/integration
```

Pri pouziti existujicich zavislosti zachovat lockfiles a schvalene feeds.
Nespoustet CI krok, ktery znovu resolvuje zavislosti proti verejnemu registry,
jako workaround omezeni spravovaneho stroje.

### CONSTRAINTS

Jednat v `refactor-rust` v rozsahu Aktivace a Git workflow. Vlastni testovaci
data a procesy oznacit tak, aby bylo mozne uklidit pouze je. Existujici
aplikaci ani osobni stav pri mereni nerestartovat nebo menit.

Cloveka pozadat pouze o login/consent, schvaleny zdroj nebo instalaci chybejiciho
toolingu, placene operace, produkcni zmeny nebo zmenu cilu a limitu.
Nedostupne prerequisites nezakryvat teoretickym vysledkem.

Fixni jsou cil, hypotezy, checky, prahy a vstupy. Materialni zmena potrebuje
novou schvalenou verzi. Git push s neznamym vysledkem overit read-backem
remote ref pred opakovanim; nikdy nepouzivat force-push.

### STAGES

Preflight -> baseline -> nejmensi funkcni Windows pilot -> backend pilot ->
porovnani a uklid. Kompletni migrace je samostatny navazujici build cil,
s etapami a neprekrocitelnymi podminkami uvedenymi vyse.

Priklad opravne vetve: pokud pamet roste kvuli neomezene audio fronte, opravit
omezeni a backpressure, potom zopakovat pametovy test i kontrolu zachovani
vzorku. Nezahazovat audio kvuli lepsim cislum.

### STOP-CAPS

Navrzeny limit pilotu: 16 hodin aktivni prace, nejvyse 72 hodin celkem a tri
opravne cykly, vcetne mereni a uklidu. Bez novych placenych sluzeb.
Tyto pilotni limity nejsou terminem dokonceni cele migrace.

`DONE` pouze s dolozenymi verdikty a splnenymi checky; jinak `BLOCKED`,
`CAPPED` nebo `CANCELLED`. Pri ukonceni zachovat neuplne podklady, presne
chybejici dukazy a bezpecny navazujici krok. Limit neni alternativni uspech.

Prvni mereni ma potvrdit, zda uspora pretrva se zapnutym recovery a streamingem.

## Stav

1. listopadu 2026: vetev `refactor-rust` vytvorena z `25390dc`.
Plan ulozen. Rust 1.99.1, Cargo, rustfmt a clippy jsou jiz nainstalovane,
ale jejich adresar nebyl v PATH relace. Docker CLI je dostupne; Docker engine
nebezi. Linuxove mereni a schvaleny Cargo registry pro externi zavislosti
nejsou zatim overeny. Pilotni checky C01-C04 jsou `NOT_RUN`.

Prvni inkrement: dependency-free audio zaklad Rustu a read-only procesove
mereni. Neni to jeste nativni tray aplikace ani nahrada backendu.
