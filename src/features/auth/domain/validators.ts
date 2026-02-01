import { AuthError, AuthErrorCode } from './errors';

const EMAIL_REGEX = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
const MIN_PASSWORD_LENGTH = 12;

/**
 * Валидация email адреса
 */
export function validateEmail(email: string): void {
  if (!email || !email.trim()) {
    throw new AuthError(AuthErrorCode.ValidationError, 'Email обязателен');
  }

  if (!EMAIL_REGEX.test(email)) {
    throw new AuthError(AuthErrorCode.ValidationError, 'Некорректный формат email');
  }
}

/**
 * Валидация пароля
 */
export function validatePassword(password: string): void {
  if (!password) {
    throw new AuthError(AuthErrorCode.ValidationError, 'Пароль обязателен');
  }

  if (password.length < MIN_PASSWORD_LENGTH) {
    throw new AuthError(
      AuthErrorCode.PasswordWeak,
      `Пароль должен содержать минимум ${MIN_PASSWORD_LENGTH} символов`
    );
  }
}

/**
 * Валидация кода подтверждения
 */
export function validateVerificationCode(code: string): void {
  if (!code || !code.trim()) {
    throw new AuthError(AuthErrorCode.ValidationError, 'Код подтверждения обязателен');
  }

  if (!/^\d{6}$/.test(code)) {
    throw new AuthError(AuthErrorCode.ValidationError, 'Код должен состоять из 6 цифр');
  }
}

/**
 * Проверка что email валидный (без throw)
 */
export function isValidEmail(email: string): boolean {
  return EMAIL_REGEX.test(email);
}

/**
 * Проверка что пароль достаточно сильный (без throw)
 */
export function isValidPassword(password: string): boolean {
  return password.length >= MIN_PASSWORD_LENGTH;
}
