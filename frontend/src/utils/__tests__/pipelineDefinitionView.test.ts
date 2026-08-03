import { describe, expect, it } from 'vitest'
import {
  collectSecretNamesFromYaml,
  concurrencyFromYaml,
  parseDurationToSeconds,
  yamlStepsToDefTasks,
} from '../pipelineDefinitionView'
import type { YamlParsedData } from '@/api/pipelines'

describe('parseDurationToSeconds', () => {
  it('parses s/m/h', () => {
    expect(parseDurationToSeconds('30s')).toBe(30)
    expect(parseDurationToSeconds('5m')).toBe(300)
    expect(parseDurationToSeconds('1h')).toBe(3600)
    expect(parseDurationToSeconds('45')).toBe(45)
  })

  it('returns 0 for empty/invalid', () => {
    expect(parseDurationToSeconds('')).toBe(0)
    expect(parseDurationToSeconds(null)).toBe(0)
    expect(parseDurationToSeconds('10x')).toBe(0)
  })
})

describe('yamlStepsToDefTasks', () => {
  it('maps YAML steps to definition cards', () => {
    const tasks = yamlStepsToDefTasks([
      {
        name: 'build',
        uses: 'docker',
        run: 'echo hi',
        image: 'alpine',
        volumes: '/a:/b, /c:/d',
        host: 'web1',
        host_labels: 'env=prod',
        dir: '/work',
        timeout: '2m',
        retry: 1,
        continue_on_error: true,
        privileged: true,
        secrets: ['TOKEN'],
        registry_secret: '${secrets.REG}',
      },
    ])
    expect(tasks[0]).toMatchObject({
      name: 'build',
      exec_type: 'docker',
      command_template: 'echo hi',
      image: 'alpine',
      volumes: ['/a:/b', '/c:/d'],
      host_names: 'web1',
      host_labels: 'env=prod',
      work_dir: '/work',
      timeout_seconds: 120,
      retry_count: 1,
      fail_strategy: 'continue',
      privileged: true,
      step_secrets: 'TOKEN',
      registry_secret: '${secrets.REG}',
    })
  })
})

describe('concurrencyFromYaml / collectSecretNamesFromYaml', () => {
  const base: YamlParsedData = { steps: [] }

  it('reads concurrency_config', () => {
    expect(
      concurrencyFromYaml({
        ...base,
        concurrency_config: {
          group: 'deploy',
          mode: 'queue',
          max_queue: 5,
          queue_timeout: '10m',
        },
      }),
    ).toEqual({
      configured: true,
      group: 'deploy',
      mode: 'queue',
      maxQueue: 5,
      queueTimeoutSec: 600,
    })
  })

  it('collects secrets from list and refs', () => {
    const names = collectSecretNamesFromYaml({
      secrets: ['A'],
      steps: [
        {
          secrets: ['B'],
          registry_secret: '${secrets.REG}',
          config_source: 'OSS_SRC',
        },
      ],
    })
    expect(names.sort()).toEqual(['A', 'B', 'OSS_SRC', 'REG'])
  })
})
