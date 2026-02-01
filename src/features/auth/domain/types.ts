// Состояние авторизации
export type AuthStatus =
  | 'idle'              // Начальное состояние
  | 'loading'           // Загрузка
  | 'authenticated'     // Авторизован
  | 'unauthenticated'   // Не авторизован
  | 'needs_verification' // Требуется подтверждение email
  | 'error';            // Ошибка
