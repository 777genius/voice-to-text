/**
 * Value object представляющий пользователя
 * Содержит только необходимые для UI данные
 */
export interface User {
  readonly id: string;
  readonly email: string;
  readonly emailVerified: boolean;
}

export function createUser(data: {
  id: string;
  email: string;
  emailVerified: boolean;
}): User {
  return Object.freeze({
    id: data.id,
    email: data.email,
    emailVerified: data.emailVerified,
  });
}

export function isValidEmail(email: string): boolean {
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email);
}
