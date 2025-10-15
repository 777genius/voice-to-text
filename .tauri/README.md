# Signing Keys for Updates

Эта директория содержит ключи для подписи обновлений приложения.

## Генерация ключей

```bash
# Сгенерировать новую пару ключей
pnpm tauri signer generate -w .tauri/voice-to-text.key

# Команда запросит пароль - можно оставить пустым (Enter)
# Будут созданы:
# - .tauri/voice-to-text.key (приватный ключ) - НЕ КОММИТИТЬ!
# - .tauri/voice-to-text.pub (публичный ключ) - копировать в tauri.conf.json
```

## Использование в GitHub Actions

Добавьте приватный ключ в GitHub Secrets:
1. Settings → Secrets and variables → Actions
2. New repository secret:
   - Name: `TAURI_SIGNING_PRIVATE_KEY`
   - Value: содержимое файла `.tauri/voice-to-text.key`
3. New repository secret (если используется пароль):
   - Name: `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
   - Value: ваш пароль

## Публичный ключ

Публичный ключ нужно вставить в `tauri.conf.json`:
```json
{
  "plugins": {
    "updater": {
      "pubkey": "СОДЕРЖИМОЕ_voice-to-text.pub"
    }
  }
}
```
