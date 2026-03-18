# RemoteLink

Acesso remoto peer-to-peer — self-contained, sem dependencias, Windows e Linux.

## Download

👉 **[Releases — clique aqui para baixar](https://github.com/geraldohumberto/remote-link/releases)**

| Arquivo | Sistema | Uso |
|---|---|---|
| `remote-link-linux` | Linux | Peer — cliente + host |
| `remote-link-windows.exe` | Windows | Peer — cliente + host |
| `remote-link-relay-linux` | Linux | Servidor de ponte (opcional) |
| `remote-link-relay-windows.exe` | Windows | Servidor de ponte (opcional) |

## Como usar

**Linux:**
```bash
chmod +x remote-link-linux
./remote-link-linux
```

**Windows:** duplo clique em `remote-link-windows.exe`

Na primeira execucao o app pede para definir uma senha.

## Tecnologia

Rust · egui · tokio · xcap · enigo
