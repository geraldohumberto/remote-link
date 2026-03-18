# CLAUDE.md — remote-link

Repositório: https://github.com/geraldohumberto/remote-link

Este arquivo instrui o Claude Code a montar, compilar e corrigir o projeto.

---

## O que é este projeto

RemoteLink — acesso remoto peer-to-peer em Rust.
- Binário **self-contained** (~15-20MB), sem instalar nada na máquina destino
- UI em `egui` (OpenGL/DirectX embutido — zero dependência do sistema)
- Sistema tray (ícone na bandeja, não ocupa espaço na área de trabalho)
- Cross-platform: Windows e Linux
- Senha configurada na primeira execução, salva localmente

### 4 arquivos finais (gerados pelo GitHub Actions):
- `remote-link-linux` — peer Linux (cliente + host)
- `remote-link-windows.exe` — peer Windows (cliente + host)
- `remote-link-relay-linux` — servidor de ponte Linux
- `remote-link-relay-windows.exe` — servidor de ponte Windows

---

## Estrutura do projeto

```
remote-link/
├── peer/                      ← binário principal (cliente + host)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs            ← UI egui + tray + entry point
│       ├── server.rs          ← aceita conexões, captura tela
│       ├── client.rs          ← conecta em peers remotos
│       ├── protocol.rs        ← protocolo TCP binário
│       ├── capture.rs         ← captura de tela (xcap)
│       ├── input.rs           ← mouse e teclado (enigo)
│       └── config.rs          ← config persistente JSON
├── relay/                     ← servidor de ponte (sem UI)
│   ├── Cargo.toml
│   └── src/
│       └── main.rs
└── .github/
    └── workflows/
        └── build.yml          ← GitHub Actions: compila os 4 binários
```

---

## PASSO 1 — Instalar dependências do sistema

### Detectar o SO e executar o comando correto:

**Ubuntu/Debian:**
```bash
sudo apt-get update && sudo apt-get install -y \
  pkg-config libgtk-3-dev libxdo-dev libx11-dev \
  libxcb1-dev libxrandr-dev libdbus-1-dev libssl-dev \
  libglib2.0-dev libpango1.0-dev libatk1.0-dev \
  libgdk-pixbuf2.0-dev libgl1-mesa-dev libegl1-mesa-dev
```

**Arch/Garuda/Manjaro:**
```bash
sudo pacman -Sy --noconfirm \
  base-devel pkg-config gtk3 libxdo xdotool \
  openssl mesa glib2 pango atk gdk-pixbuf2
```

**Windows:** verificar se Visual Studio Build Tools está instalado.
Se não estiver, instruir o usuário a instalar antes de continuar.

---

## PASSO 2 — Instalar Rust (se não estiver instalado)

```bash
rustc --version || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && source ~/.cargo/env
```

---

## PASSO 3 — Compilar o peer

```bash
cd peer
cargo build --release
```

### Erros comuns e como resolver:

**`pkg-config not found`:**
```bash
# Linux
sudo apt install pkg-config || sudo pacman -S pkg-config
```

**`libgtk-3 not found` / `gtk/gtk.h not found`:**
```bash
sudo apt install libgtk-3-dev || sudo pacman -S gtk3
```

**`libxdo not found`:**
```bash
sudo apt install libxdo-dev || sudo pacman -S libxdo
```

**`cannot find -lGL`:**
```bash
sudo apt install libgl1-mesa-dev || sudo pacman -S mesa
```

**Erro de versão de crate / incompatibilidade:** rodar `cargo update` e tentar novamente.

**Qualquer outro erro de compilação:** ler a mensagem completa, identificar o arquivo e linha, corrigir o código e rodar `cargo check` antes do `cargo build --release`.

---

## PASSO 4 — Compilar o relay

```bash
cd ../relay
cargo build --release
```

---

## PASSO 5 — Verificar binários gerados

```bash
ls -lh peer/target/release/remote-link
ls -lh relay/target/release/remote-link-relay
```

Ambos devem existir e ter tamanho > 5MB.

---

## PASSO 6 — Testar execução rápida

```bash
# Inicia o peer em background por 3 segundos pra ver se não crasha
timeout 3 ./peer/target/release/remote-link || true
echo "Teste OK"
```

---

## PASSO 7 — Fazer push para o GitHub

```bash
git add .
git commit -m "feat: projeto inicial RemoteLink"
git push origin main
```

---

## PASSO 8 — Criar tag para disparar o build no GitHub Actions

```bash
git tag v0.1.0
git push origin v0.1.0
```

Após o push da tag, o GitHub Actions vai:
1. Compilar os 4 binários (Linux + Windows)
2. Criar uma Release automática em: https://github.com/geraldohumberto/remote-link/releases
3. Os 4 arquivos prontos ficam disponíveis para download

---

## PASSO 9 — Informar o usuário

Após tudo concluído, exibir:

```
══════════════════════════════════════════════════════
  RemoteLink — Deploy completo!
══════════════════════════════════════════════════════

  GitHub Actions vai gerar os 4 binários em ~10 min.
  Acompanhe em:
  https://github.com/geraldohumberto/remote-link/actions

  Quando terminar, os arquivos estarão em:
  https://github.com/geraldohumberto/remote-link/releases

  Arquivos gerados:
    remote-link-linux         → peer Linux
    remote-link-windows.exe   → peer Windows
    remote-link-relay-linux   → relay Linux
    remote-link-relay-windows.exe → relay Windows

  Como distribuir:
    Mande o link da release para quem vai usar.
    A pessoa baixa o arquivo certo para o SO dela e executa.
    Não precisa instalar nada.

  Na primeira execução o app pede pra definir uma senha.
  Essa senha fica salva em ~/.remote-link.json
══════════════════════════════════════════════════════
```

---

## Observações importantes para o Claude Code

- Sempre rodar comandos dentro da pasta correta (`peer/` ou `relay/`)
- Após qualquer correção de código, rodar `cargo check` antes do `cargo build --release`
- Não modificar o `build.yml` a menos que solicitado
- Se o usuário estiver no Windows, adaptar todos os paths e comandos
- O arquivo de config é criado automaticamente em `~/.remote-link.json` na primeira execução
- A senha padrão inicial é `remotelink123` — o app pede pra trocar na primeira execução
