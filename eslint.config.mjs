import js from '@eslint/js';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  {
    ignores: [
      'dist/**',
      'node_modules/**',
      // Rust build output, including the JS shims tauri-build generates.
      'src-tauri/target/**',
      // Generated behavioural vectors.
      'spec/**'
    ]
  },
  js.configs.recommended,
  ...tseslint.configs.recommendedTypeChecked,
  {
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname }
    },
    rules: {
      // ---- Keep the source runnable by Node's strip-only TypeScript ----
      // Node erases types but never generates code, so any construct that
      // needs emitted JavaScript will parse in tsc and then fail at runtime.
      // Tests import the source directly, so this is a hard constraint rather
      // than a style preference.
      'no-restricted-syntax': [
        'error',
        {
          selector: 'TSEnumDeclaration',
          message: 'enum requires code generation; use a union type or an object literal.'
        },
        {
          selector: 'TSModuleDeclaration[kind="namespace"]',
          message: 'namespace requires code generation; use modules.'
        },
        {
          selector: 'TSParameterProperty',
          message:
            'constructor parameter properties require code generation; declare the field and assign it.'
        },
        {
          selector: 'TSImportEqualsDeclaration',
          message: 'import = requires code generation; use an ES import.'
        }
      ],

      // ---- Fail loudly rather than silently ----
      // A bare `catch {}` around a write is what turns a full disk into a
      // permanent, invisible loss of the user's settings.
      'no-empty': ['error', { allowEmptyCatch: false }],
      '@typescript-eslint/no-floating-promises': 'error',
      '@typescript-eslint/no-misused-promises': 'error',
      '@typescript-eslint/require-await': 'error',
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/no-unsafe-assignment': 'error',
      '@typescript-eslint/no-unsafe-member-access': 'error',
      '@typescript-eslint/no-unsafe-argument': 'error',
      '@typescript-eslint/no-unsafe-return': 'error',
      '@typescript-eslint/switch-exhaustiveness-check': 'error',
      '@typescript-eslint/consistent-type-imports': 'error',
      eqeqeq: ['error', 'always', { null: 'ignore' }],
      'no-console': 'off',

      // ---- Renderer safety ----
      // Every innerHTML site is a place where one forgotten escape turns a
      // game folder name into markup. The renderer builds DOM nodes instead.
      'no-restricted-properties': [
        'error',
        { object: 'document', property: 'write', message: 'Build DOM nodes instead.' }
      ]
    }
  },
  {
    // Tests may lean on non-null assertions and fixture casts for brevity.
    files: ['test/**/*.ts'],
    rules: {
      '@typescript-eslint/no-non-null-assertion': 'off',
      '@typescript-eslint/no-unsafe-assignment': 'off',
      '@typescript-eslint/no-unsafe-member-access': 'off',
      '@typescript-eslint/no-unsafe-argument': 'off',
      '@typescript-eslint/no-unsafe-call': 'off',
      '@typescript-eslint/no-unnecessary-type-assertion': 'off',
      // node:test's `test()` returns a promise that callers are not meant to
      // await at the top level; the runner tracks it. Requiring `void` on all
      // seventy of them would be noise, not safety.
      '@typescript-eslint/no-floating-promises': 'off',
      // An async test body with no await is still the right signature.
      '@typescript-eslint/require-await': 'off'
    }
  },
  {
    files: ['**/*.mjs'],
    ...tseslint.configs.disableTypeChecked,
    languageOptions: {
      // These are plain ESM scripts, not part of the TypeScript program, so the
      // type-aware project service must not try to resolve them.
      parserOptions: { projectService: false, project: false },
      globals: {
        process: 'readonly',
        console: 'readonly',
        Buffer: 'readonly',
        URL: 'readonly',
        TextDecoder: 'readonly',
        TextEncoder: 'readonly',
        setTimeout: 'readonly',
        clearTimeout: 'readonly',
        setInterval: 'readonly',
        clearInterval: 'readonly',
        __dirname: 'readonly',
        Infinity: 'readonly'
      }
    }
  }
);
