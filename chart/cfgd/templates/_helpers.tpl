{{/*
Expand the name of the chart.
*/}}
{{- define "cfgd.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "cfgd.fullname" -}}
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
{{- define "cfgd.labels" -}}
helm.sh/chart: {{ .Chart.Name }}-{{ .Chart.Version | replace "+" "_" }}
{{ include "cfgd.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels (base)
*/}}
{{- define "cfgd.selectorLabels" -}}
app.kubernetes.io/name: {{ include "cfgd.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Operator selector labels
*/}}
{{- define "cfgd.operatorSelectorLabels" -}}
{{ include "cfgd.selectorLabels" . }}
app.kubernetes.io/component: operator
{{- end }}

{{/*
Agent selector labels
*/}}
{{- define "cfgd.agentSelectorLabels" -}}
{{ include "cfgd.selectorLabels" . }}
app.kubernetes.io/component: agent
{{- end }}

{{/*
CSI driver selector labels
*/}}
{{- define "cfgd.csiSelectorLabels" -}}
{{ include "cfgd.selectorLabels" . }}
app.kubernetes.io/component: csi-driver
{{- end }}

{{/*
Service account name
*/}}
{{- define "cfgd.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "cfgd.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Agent service account name
*/}}
{{- define "cfgd.agentServiceAccountName" -}}
{{ include "cfgd.fullname" . }}-agent
{{- end }}

{{/*
Webhook certificate secret name
*/}}
{{- define "cfgd.webhookCertSecret" -}}
{{ include "cfgd.fullname" . }}-webhook-tls
{{- end }}

{{/*
Webhook certificate resource name (for cert-manager Certificate and inject-ca-from annotation)
*/}}
{{- define "cfgd.webhookCertName" -}}
{{ include "cfgd.fullname" . }}-webhook-tls
{{- end }}
