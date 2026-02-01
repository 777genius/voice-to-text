import { describe, it, expect } from 'vitest';
import {
  validateEmail,
  validatePassword,
  validateVerificationCode,
  isValidEmail,
  isValidPassword,
} from './validators';
import { AuthError, AuthErrorCode } from './errors';

describe('validators', () => {
  describe('validateEmail', () => {
    it('проходит для корректного email', () => {
      expect(() => validateEmail('test@example.com')).not.toThrow();
      expect(() => validateEmail('user.name@domain.org')).not.toThrow();
    });

    it('выбрасывает ошибку для пустого email', () => {
      expect(() => validateEmail('')).toThrow(AuthError);
      expect(() => validateEmail('   ')).toThrow(AuthError);
    });

    it('выбрасывает ошибку для некорректного формата', () => {
      expect(() => validateEmail('invalid')).toThrow(AuthError);
      expect(() => validateEmail('no-at-sign.com')).toThrow();
      expect(() => validateEmail('@nodomain.com')).toThrow();
    });

    it('возвращает правильный код ошибки', () => {
      try {
        validateEmail('invalid');
      } catch (e) {
        expect((e as AuthError).code).toBe(AuthErrorCode.ValidationError);
      }
    });
  });

  describe('validatePassword', () => {
    it('проходит для пароля >= 12 символов', () => {
      expect(() => validatePassword('123456789012')).not.toThrow();
      expect(() => validatePassword('verylongpassword123')).not.toThrow();
    });

    it('выбрасывает ошибку для короткого пароля', () => {
      expect(() => validatePassword('12345678901')).toThrow(AuthError);
      expect(() => validatePassword('short')).toThrow();
    });

    it('выбрасывает ошибку для пустого пароля', () => {
      expect(() => validatePassword('')).toThrow(AuthError);
    });

    it('возвращает код PasswordWeak для короткого пароля', () => {
      try {
        validatePassword('short');
      } catch (e) {
        expect((e as AuthError).code).toBe(AuthErrorCode.PasswordWeak);
      }
    });
  });

  describe('validateVerificationCode', () => {
    it('проходит для 6-значного кода', () => {
      expect(() => validateVerificationCode('123456')).not.toThrow();
      expect(() => validateVerificationCode('000000')).not.toThrow();
    });

    it('выбрасывает ошибку для пустого кода', () => {
      expect(() => validateVerificationCode('')).toThrow(AuthError);
      expect(() => validateVerificationCode('   ')).toThrow();
    });

    it('выбрасывает ошибку для некорректного формата', () => {
      expect(() => validateVerificationCode('12345')).toThrow(); // 5 цифр
      expect(() => validateVerificationCode('1234567')).toThrow(); // 7 цифр
      expect(() => validateVerificationCode('abcdef')).toThrow(); // буквы
      expect(() => validateVerificationCode('12345a')).toThrow(); // смешанный
    });
  });

  describe('isValidEmail', () => {
    it('возвращает true для корректного email', () => {
      expect(isValidEmail('test@example.com')).toBe(true);
    });

    it('возвращает false для некорректного email', () => {
      expect(isValidEmail('invalid')).toBe(false);
      expect(isValidEmail('')).toBe(false);
    });
  });

  describe('isValidPassword', () => {
    it('возвращает true для пароля >= 12 символов', () => {
      expect(isValidPassword('123456789012')).toBe(true);
    });

    it('возвращает false для короткого пароля', () => {
      expect(isValidPassword('short')).toBe(false);
      expect(isValidPassword('')).toBe(false);
    });
  });
});
