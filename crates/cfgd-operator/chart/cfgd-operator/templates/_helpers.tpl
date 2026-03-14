{{/*
Expand the name of the chart.
*/}}
{{- define "cfgd-operator.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "cfgd-operator.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "cfgd-operator.labels" -}}
helm.sh/chart: {{ include "cfgd-operator.name" . }}-{{ .Chart.Version }}
{{ include "cfgd-operator.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "cfgd-operator.selectorLabels" -}}
app.kubernetes.io/name: {{ include "cfgd-operator.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Service account name
*/}}
{{- define "cfgd-operator.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "cfgd-operator.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Webhook certificate secret name
*/}}
{{- define "cfgd-operator.webhookCertSecret" -}}
{{ include "cfgd-operator.fullname" . }}-webhook-tls
{{- end }}
